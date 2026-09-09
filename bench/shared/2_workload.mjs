import { createHash } from "node:crypto";

export const seed = "0x6d65726375727901";

export const summaryQuery = `
  SELECT dimension.group_id,
         count(*) AS row_count,
         sum(fact.amount::bigint * dimension.factor::bigint) AS weighted_sum
    FROM fact
    JOIN dimension USING (group_id)
   GROUP BY dimension.group_id
`;

export const distinctQuery = `
  SELECT DISTINCT group_id, amount
    FROM fact
`;

export function groupsFor(rowCount) {
  return Math.max(4, Math.min(128, Math.ceil(Math.sqrt(rowCount))));
}

export function validateCase(rowCount, batchSize) {
  if (!Number.isSafeInteger(rowCount) || rowCount < 8) {
    throw new Error(`rows must be an integer >= 8: ${rowCount}`);
  }
  if (!Number.isSafeInteger(batchSize) || batchSize < 1 || rowCount < batchSize * 2 + 4) {
    throw new Error(`batch must be positive and rows must be >= 2 * batch + 4: ${batchSize}`);
  }
}

function amountFor(id) {
  return (id * 37) % 1000 - 500;
}

function insertedAmountFor(id) {
  return (id * 53) % 1200 - 600;
}

export function makeOracle(rowCount, batchSize) {
  validateCase(rowCount, batchSize);
  const groupCount = groupsFor(rowCount);
  const dimensions = new Map();
  const facts = new Map();
  for (let groupId = 0; groupId < groupCount; groupId += 1) {
    dimensions.set(groupId, groupId % 7 + 1);
  }
  facts.set(1, { groupId: 0, amount: 1_000_000_000 });
  facts.set(2, { groupId: 0, amount: 1_000_000_000 });
  for (let id = 3; id <= rowCount; id += 1) {
    facts.set(id, { groupId: id % groupCount, amount: amountFor(id) });
  }

  const apply = {
    initial() {},
    insert_batch() {
      for (let offset = 1; offset <= batchSize; offset += 1) {
        const id = rowCount + offset;
        facts.set(id, { groupId: id % groupCount, amount: insertedAmountFor(id) });
      }
    },
    delete_batch() {
      for (let offset = 0; offset < batchSize; offset += 1) facts.delete(3 + offset * 2);
    },
    update_batch() {
      for (let offset = 0; offset < batchSize; offset += 1) {
        const id = 4 + offset * 2;
        const fact = facts.get(id);
        if (fact) facts.set(id, { groupId: (fact.groupId + 1) % groupCount, amount: fact.amount + 17 });
      }
    },
    dimension_fanout() {
      dimensions.set(0, dimensions.get(0) + 3);
    },
    duplicate_delete_one() {
      facts.delete(1);
    },
    duplicate_delete_two() {
      facts.delete(2);
    },
  };

  const snapshot = () => {
    const summary = new Map();
    const distinct = new Set();
    for (const { groupId, amount } of facts.values()) {
      const factor = dimensions.get(groupId);
      const current = summary.get(groupId) ?? { count: 0n, sum: 0n };
      current.count += 1n;
      current.sum += BigInt(amount) * BigInt(factor);
      summary.set(groupId, current);
      distinct.add(`${groupId}\t${amount}`);
    }
    const summaryRows = [...summary]
      .sort(([left], [right]) => left - right)
      .map(([groupId, value]) => [String(groupId), String(value.count), String(value.sum)]);
    const distinctRows = [...distinct]
      .map((entry) => entry.split("\t"))
      .sort((left, right) => Number(left[0]) - Number(right[0]) || Number(left[1]) - Number(right[1]));
    return checksumRows(summaryRows, distinctRows);
  };

  return { apply, snapshot };
}

export function checksumRows(summaryRows, distinctRows) {
  const canonical = [
    ...summaryRows.map((row) => `S\t${row.join("\t")}`),
    ...distinctRows.map((row) => `D\t${row.join("\t")}`),
  ].join("\n");
  return {
    checksum: createHash("sha256").update(canonical).digest("hex"),
    summary_rows: summaryRows.length,
    distinct_rows: distinctRows.length,
    transfer_bytes: Buffer.byteLength(canonical),
  };
}

export function mutationSql(rowCount, batchSize, groupCount) {
  return {
    initial: "SELECT 1",
    insert_batch: `
      INSERT INTO fact(id, group_id, amount)
      SELECT id, id % ${groupCount}, (id * 53) % 1200 - 600
        FROM generate_series(${rowCount + 1}, ${rowCount + batchSize}) AS id
    `,
    delete_batch: `
      DELETE FROM fact
       WHERE id >= 3 AND id < ${3 + batchSize * 2} AND (id - 3) % 2 = 0
    `,
    update_batch: `
      UPDATE fact
         SET group_id = (group_id + 1) % ${groupCount}, amount = amount + 17
       WHERE id >= 4 AND id < ${4 + batchSize * 2} AND (id - 4) % 2 = 0
    `,
    dimension_fanout: "UPDATE dimension SET factor = factor + 3 WHERE group_id = 0",
    duplicate_delete_one: "DELETE FROM fact WHERE id = 1",
    duplicate_delete_two: "DELETE FROM fact WHERE id = 2",
  };
}

export function recursiveCases(profile) {
  const nodeCount = profile === "smoke" ? 16 : 48;
  return [
    { family: "chain", nodeCount },
    { family: "ring", nodeCount },
  ];
}

export function recursiveEdges(family, nodeCount) {
  const edges = [];
  if (family === "chain") {
    for (let node = 0; node + 1 < nodeCount; node += 1) edges.push([node, node + 1]);
  } else if (family === "ring") {
    for (let node = 0; node < nodeCount; node += 1) edges.push([node, (node + 1) % nodeCount]);
  } else {
    throw new Error(`unknown recursive family: ${family}`);
  }
  return edges;
}

export function recursiveChecksum(family, nodeCount) {
  const rows = [];
  if (family === "chain") {
    for (let source = 0; source < nodeCount; source += 1) {
      for (let target = source + 1; target < nodeCount; target += 1) rows.push(`${source}\t${target}`);
    }
  } else if (family === "ring") {
    for (let source = 0; source < nodeCount; source += 1) {
      for (let target = 0; target < nodeCount; target += 1) rows.push(`${source}\t${target}`);
    }
  }
  rows.sort((left, right) => {
    const [leftSource, leftTarget] = left.split("\t").map(Number);
    const [rightSource, rightTarget] = right.split("\t").map(Number);
    return leftSource - rightSource || leftTarget - rightTarget;
  });
  const canonical = rows.join("\n");
  return {
    checksum: createHash("sha256").update(canonical).digest("hex"),
    derived: rows.length,
    transfer_bytes: Buffer.byteLength(canonical),
  };
}
