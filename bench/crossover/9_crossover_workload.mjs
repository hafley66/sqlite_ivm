import { createHash } from "node:crypto";

export const crossoverSeed = "0x706f737467726573";

export const crossoverSummaryQuery = `
  SELECT dimension.group_id,
         count(*) AS row_count,
         sum(fact.amount::bigint * dimension.factor::bigint) AS weighted_sum
    FROM fact
    JOIN dimension USING (group_id)
   GROUP BY dimension.group_id
`;

export const crossoverStates = [
  "initial",
  "insert_batch",
  "delete_batch",
  "update_batch",
  "dimension_fanout",
];

export function crossoverGroupsFor(rowCount) {
  return Math.max(8, Math.min(128, Math.ceil(Math.sqrt(rowCount))));
}

export function validateCrossoverCase(rowCount, batchSize, fanout) {
  for (const [name, value] of [["rows", rowCount], ["batch", batchSize], ["fanout", fanout]]) {
    if (!Number.isSafeInteger(value) || value < 1) throw new Error(`${name} must be a positive integer: ${value}`);
  }
  if (fanout + batchSize * 2 > rowCount) {
    throw new Error(`rows must cover fanout plus two mutation batches: ${rowCount} < ${fanout + batchSize * 2}`);
  }
}

function amountFor(id) {
  return (id * 37) % 1000 - 500;
}

function insertedAmountFor(id) {
  return (id * 53) % 1200 - 600;
}

function initialGroupFor(id, fanout, groupCount) {
  return id <= fanout ? 0 : 1 + ((id - fanout - 1) % (groupCount - 1));
}

function checksumSummary(summary) {
  const rows = [...summary]
    .filter(([, value]) => value.count > 0n)
    .sort(([left], [right]) => left - right)
    .map(([groupId, value]) => [String(groupId), String(value.count), String(value.sum)]);
  const canonical = rows.map((row) => `S\t${row.join("\t")}`).join("\n");
  return {
    checksum: createHash("sha256").update(canonical).digest("hex"),
    output_rows: rows.length,
    output_bytes: Buffer.byteLength(canonical),
    summary: rows,
  };
}

export function makeCrossoverOracle(rowCount, batchSize, fanout) {
  validateCrossoverCase(rowCount, batchSize, fanout);
  const groupCount = crossoverGroupsFor(rowCount);
  const dimensions = new Map();
  const facts = new Map();
  for (let groupId = 0; groupId < groupCount; groupId += 1) dimensions.set(groupId, groupId % 7 + 1);
  for (let id = 1; id <= rowCount; id += 1) {
    facts.set(id, { groupId: initialGroupFor(id, fanout, groupCount), amount: amountFor(id) });
  }

  const apply = {
    initial() {},
    insert_batch() {
      for (let offset = 1; offset <= batchSize; offset += 1) {
        const id = rowCount + offset;
        facts.set(id, { groupId: 1 + ((offset - 1) % (groupCount - 1)), amount: insertedAmountFor(id) });
      }
    },
    delete_batch() {
      for (let id = fanout + 1; id <= fanout + batchSize; id += 1) facts.delete(id);
    },
    update_batch() {
      for (let id = fanout + batchSize + 1; id <= fanout + batchSize * 2; id += 1) {
        const fact = facts.get(id);
        facts.set(id, {
          groupId: 1 + (fact.groupId % (groupCount - 1)),
          amount: fact.amount + 17,
        });
      }
    },
    dimension_fanout() {
      dimensions.set(0, dimensions.get(0) + 3);
    },
  };

  function snapshot() {
    const summary = new Map();
    for (const { groupId, amount } of facts.values()) {
      const value = summary.get(groupId) ?? { count: 0n, sum: 0n };
      value.count += 1n;
      value.sum += BigInt(amount) * BigInt(dimensions.get(groupId));
      summary.set(groupId, value);
    }
    return checksumSummary(summary);
  }

  function inputRows() {
    return {
      dimension: [...dimensions].sort(([a], [b]) => a - b),
      fact: [...facts].sort(([a], [b]) => a - b)
        .map(([id, { groupId, amount }]) => [id, groupId, amount]),
    };
  }

  return { apply, snapshot, groupCount, inputRows };
}

export function crossoverMutationSql(rowCount, batchSize, fanout, groupCount) {
  return {
    initial: "SELECT 1",
    insert_batch: `
      INSERT INTO fact(id, group_id, amount)
      SELECT id,
             1 + ((id - ${rowCount + 1}) % ${groupCount - 1}),
             (id * 53) % 1200 - 600
        FROM generate_series(${rowCount + 1}, ${rowCount + batchSize}) AS id
    `,
    delete_batch: `DELETE FROM fact WHERE id BETWEEN ${fanout + 1} AND ${fanout + batchSize}`,
    update_batch: `
      UPDATE fact
         SET group_id = 1 + (group_id % ${groupCount - 1}), amount = amount + 17
       WHERE id BETWEEN ${fanout + batchSize + 1} AND ${fanout + batchSize * 2}
    `,
    dimension_fanout: "UPDATE dimension SET factor = factor + 3 WHERE group_id = 0",
  };
}

export function expectedAffectedRows(state, batchSize) {
  if (state === "initial") return 0;
  if (state === "dimension_fanout") return 1;
  return batchSize;
}

export function crossoverInputHash(inputs) {
  const canonical = [
    ...inputs.dimension.map((row) => `D\t${row.join("\t")}`),
    ...inputs.fact.map((row) => `F\t${row.join("\t")}`),
  ].join("\n");
  return createHash("sha256").update(canonical).digest("hex");
}

// DD receives keyed writes, not the expected output. Existing row images are
// looked up and retracted by the DD adapter inside its measured write boundary.
function keyedWrites(before, after) {
  return Object.fromEntries(["dimension", "fact"].map((rel) => {
    const old = new Map(before[rel].map((row) => [row[0], row]));
    const next = new Map(after[rel].map((row) => [row[0], row]));
    return [rel, {
      deletes: [...old.keys()].filter((key) => !next.has(key)),
      puts: [...next.values()].filter((row) => JSON.stringify(old.get(row[0])) !== JSON.stringify(row)),
    }];
  }));
}

export function makeCrossoverFixture(rowCount, batchSize, fanout, semantic = false) {
  const oracle = makeCrossoverOracle(rowCount, batchSize, fanout);
  const sql = crossoverMutationSql(rowCount, batchSize, fanout, oracle.groupCount);
  let previous = { dimension: [], fact: [] };
  const states = [];
  function append(name, inputs, mutationSql, expected, affected) {
    states.push({ name, inputs, mutation_sql: mutationSql, expected,
      input_hash: crossoverInputHash(inputs), keyed_writes: keyedWrites(previous, inputs),
      expected_affected_rows: affected,
      join_affected_rows: name === "dimension_fanout" ? fanout : affected });
    previous = inputs;
  }
  for (const name of crossoverStates) {
    oracle.apply[name]();
    const inputs = oracle.inputRows();
    const mutation = name === "insert_batch"
      ? `INSERT INTO fact(id,group_id,amount) VALUES ${inputs.fact.filter(([id]) => id > rowCount).map((row) => `(${row.join(",")})`).join(",")}`
      : sql[name];
    append(name, inputs, mutation, oracle.snapshot(), expectedAffectedRows(name, batchSize));
  }
  if (semantic) {
    const facts = new Map(previous.fact.map((row) => [row[0], row]));
    const dimensions = new Map(previous.dimension);
    function step(name, mutation, operation, affected = 1) {
      operation();
      const inputs = { dimension: [...dimensions].sort(([a], [b]) => a - b),
        fact: [...facts.values()].sort(([a], [b]) => a - b) };
      const summary = new Map();
      for (const [, group, amount] of inputs.fact) {
        if (!dimensions.has(group)) continue;
        const value = summary.get(group) ?? { count: 0n, sum: 0n };
        value.count += 1n;
        value.sum += BigInt(amount) * BigInt(dimensions.get(group));
        summary.set(group, value);
      }
      append(name, inputs, mutation, checksumSummary(summary), affected);
    }
    step("semantic_delete", "DELETE FROM fact WHERE id=1", () => facts.delete(1));
    step("semantic_reinsert", "INSERT INTO fact VALUES(1,1,7)", () => facts.set(1, [1, 1, 7]));
    step("semantic_key_move", "UPDATE fact SET id=90001,group_id=2,amount=-9 WHERE id=1", () => { facts.delete(1); facts.set(90001, [90001, 2, -9]); });
    step("semantic_dimension_insert", "INSERT INTO dimension VALUES(900,0)", () => dimensions.set(900, 0));
    step("semantic_zero_sum", "INSERT INTO fact VALUES(90002,900,17)", () => facts.set(90002, [90002, 900, 17]));
    step("semantic_last_group_delete", "DELETE FROM fact WHERE id=90002", () => facts.delete(90002));
    step("semantic_dimension_delete", "DELETE FROM dimension WHERE group_id=900", () => dimensions.delete(900));
    step("semantic_dimension_reinsert", "INSERT INTO dimension VALUES(900,-2)", () => dimensions.set(900, -2));
    for (const seed of [7, 42, 2026]) {
      let random = seed;
      const next = () => (random = (Math.imul(random, 1664525) + 1013904223) >>> 0);
      for (let index = 0; index < 50; index++) {
        const id = 1 + next() % 450, group = next() % oracle.groupCount, amount = next() % 1001 - 500;
        const operation = (next() >>> 16) % 4;
        const name = `seed_${seed}_${index}`;
        if (operation === 0) {
          step(name, `INSERT INTO fact VALUES(${id},${group},${amount}) ON CONFLICT(id) DO UPDATE SET group_id=excluded.group_id,amount=excluded.amount`, () => facts.set(id, [id, group, amount]));
        } else if (operation === 1) {
          step(name, `DELETE FROM fact WHERE id=${id}`, () => facts.delete(id), facts.has(id) ? 1 : 0);
        } else if (operation === 2) {
          step(name, `UPDATE fact SET group_id=${group},amount=${amount} WHERE id=${id}`, () => { if (facts.has(id)) facts.set(id, [id, group, amount]); }, facts.has(id) ? 1 : 0);
        } else {
          const factor = next() % 11 - 5;
          step(name, `UPDATE dimension SET factor=${factor} WHERE group_id=${group}`, () => dimensions.set(group, factor));
        }
      }
    }
    step("semantic_duplicate_supports", "INSERT INTO fact VALUES(91001,1,7),(91002,1,7)", () => {facts.set(91001,[91001,1,7]);facts.set(91002,[91002,1,7]);},2);
    step("semantic_duplicate_retract", "DELETE FROM fact WHERE id=91001", () => facts.delete(91001));
    step("semantic_multirow_dimension", "UPDATE dimension SET factor=-factor", () => {for(const [g,f] of dimensions) dimensions.set(g,f===0?0:-f);},dimensions.size);
    step("semantic_dimension_key_move", "UPDATE dimension SET group_id=901 WHERE group_id=900", () => {const f=dimensions.get(900);dimensions.delete(900);dimensions.set(901,f);});
    step("semantic_empty_facts", "DELETE FROM fact", () => facts.clear(),facts.size);
    step("semantic_empty_dimensions", "DELETE FROM dimension", () => dimensions.clear(),dimensions.size);
    step("semantic_empty_reseed_dimension", "INSERT INTO dimension VALUES(1,-2)", () => dimensions.set(1,-2));
    step("semantic_empty_reseed_fact", "INSERT INTO fact VALUES(1,1,5)", () => facts.set(1,[1,1,5]));
  }
  return { states, integer_contract: "bounded non-null integers; keyed sets; exact signed 64-bit count/sum domain" };
}
