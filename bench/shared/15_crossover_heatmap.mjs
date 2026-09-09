import { readFile, writeFile } from "node:fs/promises";

const inputPath = process.argv[2] ?? "results/crossover-full.jsonl";
const outputPath = process.argv[3] ?? "results/crossover-heatmaps.svg";
const records = (await readFile(inputPath, "utf8")).trim().split("\n").filter(Boolean).map(JSON.parse);

function median(values) {
  const sorted = values.toSorted((left, right) => left - right);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 0 ? (sorted[middle - 1] + sorted[middle]) / 2 : sorted[middle];
}

function cellKey(record) {
  return `${record.circuit ?? "aggregate"}:${record.budget}:${record.fanout}:${record.rows}:${record.batch_size}`;
}

function escapeXml(value) {
  return String(value).replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;");
}

function short(value) {
  if (value >= 1_000) return `${Number((value / 1_000).toPrecision(3))}k`;
  return String(value);
}

function blend(from, to, amount) {
  return from.map((value, index) => Math.round(value + (to[index] - value) * amount));
}

function cellColor(speedup) {
  const magnitude = Math.min(1, Math.abs(Math.log2(speedup)) / 2);
  const rgb = speedup >= 1
    ? blend([229, 231, 235], [37, 99, 235], magnitude)
    : blend([229, 231, 235], [220, 38, 38], magnitude);
  return { fill: `rgb(${rgb.join(",")})`, text: magnitude > 0.58 ? "#ffffff" : "#111827" };
}

if (process.argv.includes("--three-way") || process.argv.includes("--all-arms")) {
  const allArms=process.argv.includes("--all-arms");
  const planned = records.filter((row) => row.event === "run-metadata")
    .flatMap((row) => row.cases.map((entry) => ({ ...entry, budget: row.budget })));
  const cases = [...new Map(planned.map((row) => [cellKey(row), row])).values()];
  const triples = records.filter((row) => (allArms ? ["all-arm-run","circuit-admitted-run"].includes(row.event) : row.event === "three-way-run") && row.status === "ok" && row.all_input_output_states_match);
  const arms = allArms ? [...new Set(records.filter((r)=>r.event==="run-metadata").flatMap((r)=>r.arms))].map((a)=>[a,a,"circle"]) : [["pg_ivm_ms", "pg_ivm", "circle"], ["sqlite_affected_group_ms", "SQLite affected-group", "square"], ["dd_ms", "DD volatile", "triangle"]];
  const all = triples.flatMap((row) => arms.map(([field]) => (allArms ? row.totals[field] : row[field]))).filter(v=>Number.isFinite(v)&&v>0);
  const lowPower = all.length ? Math.floor(Math.log10(Math.min(...all))) : -1;
  const highPower = all.length ? Math.max(lowPower + 1, Math.ceil(Math.log10(Math.max(...all)))) : 3;
  const left = 375, plotWidth = 375, width = 1050, top = 105, rowHeight = arms.length*22+20;
  const height = top + cases.length * rowHeight + 60;
  const x = (value) => left + (Math.log10(value) - lowPower) / (highPower - lowPower) * plotWidth;
  const parts = [`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${height}" width="${width}" height="${height}" role="img" aria-labelledby="title desc">`,
    `<title id="title">Matched SQLite plugin and existing engine crossover</title>`,
    `<desc id="desc">Shared fixture transitions plus materialization and count, milliseconds on a logarithmic axis. Marks show median and whiskers minimum to maximum across successful exact-state matched repetitions. PostgreSQL and SQLite commit durable writes; DD is volatile.</desc>`,
    `<style>text{font-family:system-ui,sans-serif;fill:currentColor;font-size:12px}.head{font-size:16px}.small{font-size:11px}line,path,rect,circle{stroke:currentColor}.grid{opacity:.15}.mark{fill:currentColor}</style>`,
    `<text class="head" x="16" y="25">Matched circuit states: write through result materialization</text>`,
    `<text x="16" y="48">PG/SQLite durable; DD volatile. Exact validation outside timing. Median [min, max] ms.</text>`,
    `<text x="16" y="76">rows / batch / fanout</text><text x="${left}" y="76">milliseconds (log scale)</text>`];
  for (let power = lowPower; power <= highPower; power++) {
    const at = x(10 ** power);
    parts.push(`<line class="grid" x1="${at}" x2="${at}" y1="90" y2="${height - 55}"/><text class="small" x="${at}" y="90" text-anchor="middle">${10 ** power}</text>`);
  }
  cases.forEach((cell, index) => {
    const matched = triples.filter((row) => cellKey(row) === cellKey(cell));
    const base = top + index * rowHeight;
    parts.push(`<text x="16" y="${base + 24}">${escapeXml(cell.circuit ?? "aggregate")} ${cell.rows} / ${cell.batch_size} / ${cell.fanout}</text>`,
      `<text class="small" x="16" y="${base + 42}">${escapeXml(cell.budget)}; n=${matched.length} matched trials</text>`);
    arms.forEach(([field, label, shape], armIndex) => {
      const y = base + armIndex * 22;
      parts.push(`<text x="174" y="${y + 4}">${label}</text>`);
      if (!matched.length) { parts.push(`<text x="${left}" y="${y + 4}">UNMEASURED</text>`); return; }
      const values = matched.map((row) => (allArms ? row.totals[field] : row[field])).filter(Number.isFinite);
      if(!values.length){const capability=records.find(r=>r.event==="capability" && r.maintenance===field && cellKey(r)===cellKey(cell));parts.push(`<text x="${left}" y="${y+4}">${escapeXml(capability?.status ?? "UNMEASURED")}</text>`);return;}
      const mid = median(values), low = Math.min(...values), high = Math.max(...values), center = x(mid);
      parts.push(`<line x1="${x(low)}" x2="${x(high)}" y1="${y}" y2="${y}"/>`);
      if (shape === "circle") parts.push(`<circle class="mark" cx="${center}" cy="${y}" r="3"/>`);
      if (shape === "square") parts.push(`<rect class="mark" x="${center - 3}" y="${y - 3}" width="6" height="6"/>`);
      if (shape === "triangle") parts.push(`<path class="mark" d="M${center},${y - 4} l4,7 h-8 Z"/>`);
      parts.push(`<text x="775" y="${y + 4}">${mid.toFixed(3)} [${low.toFixed(3)}, ${high.toFixed(3)}]</text>`);
    });
  });
  parts.push(`<text x="16" y="${height - 20}">Total memory cap unenforced. Native SQL includes client/server command latency; DD uses keyed in-process writes.</text></svg>`);
  await writeFile(outputPath, parts.join("\n") + "\n");
  console.log(JSON.stringify({ event: allArms ? "crossover-all-arm-chart" : "crossover-three-way-chart", status: "ok", input: inputPath, output: outputPath, cells: cases.length, successful_matched_trials: triples.length }));
  process.exit(0);
}

const planned = new Map();
for (const metadata of records.filter((record) => record.event === "run-metadata")) {
  for (const testCase of metadata.cases) {
    const value = { ...testCase, budget: metadata.budget };
    planned.set(cellKey(value), value);
  }
}
const measured = new Map();
for (const record of records.filter((entry) => entry.event === "paired-run" && entry.status === "ok" && entry.checksum_match)) {
  const key = cellKey(record);
  const values = measured.get(key) ?? [];
  values.push(record.full_query_over_ivm_speedup);
  measured.set(key, values);
}

const budgets = [...new Set([...planned.values()].map((record) => record.budget))].sort();
const fanouts = [...new Set([...planned.values()].map((record) => record.fanout))].sort((left, right) => left - right);
const rows = [...new Set([...planned.values()].map((record) => record.rows))].sort((left, right) => left - right);
const batches = [...new Set([...planned.values()].map((record) => record.batch_size))].sort((left, right) => left - right);
const panels = budgets.flatMap((budget) => fanouts.map((fanout) => ({ budget, fanout })));

const cellWidth = 78;
const cellHeight = 54;
const panelWidth = 116 + rows.length * cellWidth;
const panelHeight = 78 + batches.length * cellHeight;
const columns = 2;
const gapX = 34;
const gapY = 42;
const margin = 28;
const header = 92;
const width = margin * 2 + panelWidth * columns + gapX;
const height = header + Math.ceil(panels.length / columns) * panelHeight + (Math.ceil(panels.length / columns) - 1) * gapY + 76;
const parts = [];
parts.push(`<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}" role="img" aria-labelledby="title description">`);
parts.push(`<title id="title">PostgreSQL full-query over pg_ivm update-plus-query speedup</title>`);
parts.push(`<desc id="description">Receipt-generated heatmaps with row count horizontally, batch size vertically, and separate memory budget and exact join fanout panels. Numeric labels are medians with measured minimum to maximum ranges.</desc>`);
parts.push(`<defs><pattern id="unscheduled" width="8" height="8" patternUnits="userSpaceOnUse" patternTransform="rotate(45)"><rect width="8" height="8" fill="#f3f4f6"/><line x1="0" y1="0" x2="0" y2="8" stroke="#d1d5db" stroke-width="2"/></pattern></defs>`);
parts.push(`<rect width="100%" height="100%" fill="#ffffff"/>`);
parts.push(`<style>text{font-family:ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;fill:#111827}.title{font-size:20px;font-weight:600}.subtitle{font-size:12px;fill:#4b5563}.panel{font-size:15px;font-weight:600}.axis{font-size:11px;fill:#374151}.value{font-size:13px;font-weight:600}.range{font-size:9px}.unm{font-size:10px;fill:#4b5563}.grid{stroke:#ffffff;stroke-width:2}.border{fill:none;stroke:#9ca3af;stroke-width:1}</style>`);
parts.push(`<text class="title" x="${margin}" y="30">Full-query / pg_ivm speedup, update + query</text>`);
parts.push(`<text class="subtitle" x="${margin}" y="51">Median centered in each measured cell; second line is measured min–max. Values above 1 mean lower pg_ivm time.</text>`);
parts.push(`<text class="subtitle" x="${margin}" y="70">Total memory cap: UNENFORCED. PostgreSQL tuning profiles are shown as separate panels.</text>`);

for (let panelIndex = 0; panelIndex < panels.length; panelIndex += 1) {
  const panel = panels[panelIndex];
  const panelX = margin + (panelIndex % columns) * (panelWidth + gapX);
  const panelY = header + Math.floor(panelIndex / columns) * (panelHeight + gapY);
  const gridX = panelX + 104;
  const gridY = panelY + 54;
  parts.push(`<text class="panel" x="${panelX}" y="${panelY + 16}">${escapeXml(panel.budget)} budget · exact fanout ${panel.fanout}</text>`);
  parts.push(`<text class="axis" x="${gridX + rows.length * cellWidth / 2}" y="${panelY + 34}" text-anchor="middle">base rows</text>`);
  parts.push(`<text class="axis" x="${panelX + 10}" y="${gridY + batches.length * cellHeight / 2}" transform="rotate(-90 ${panelX + 10} ${gridY + batches.length * cellHeight / 2})" text-anchor="middle">batch</text>`);
  rows.forEach((rowCount, column) => {
    parts.push(`<text class="axis" x="${gridX + column * cellWidth + cellWidth / 2}" y="${gridY - 8}" text-anchor="middle">${short(rowCount)}</text>`);
  });
  batches.forEach((batchSize, row) => {
    parts.push(`<text class="axis" x="${gridX - 10}" y="${gridY + row * cellHeight + cellHeight / 2 + 4}" text-anchor="end">${short(batchSize)}</text>`);
    rows.forEach((rowCount, column) => {
      const key = cellKey({ ...panel, rows: rowCount, batch_size: batchSize });
      const values = measured.get(key);
      const isPlanned = planned.has(key);
      const x = gridX + column * cellWidth;
      const y = gridY + row * cellHeight;
      if (values?.length) {
        const center = median(values);
        const low = Math.min(...values);
        const high = Math.max(...values);
        const color = cellColor(center);
        parts.push(`<g><title>${escapeXml(`${panel.budget}, fanout ${panel.fanout}, ${rowCount} rows, batch ${batchSize}: ${center.toFixed(3)}x median, ${low.toFixed(3)}x to ${high.toFixed(3)}x`)}</title>`);
        parts.push(`<rect class="grid" x="${x}" y="${y}" width="${cellWidth}" height="${cellHeight}" fill="${color.fill}"/>`);
        parts.push(`<text class="value" x="${x + cellWidth / 2}" y="${y + 23}" text-anchor="middle" style="fill:${color.text}">${center.toFixed(2)}×</text>`);
        parts.push(`<text class="range" x="${x + cellWidth / 2}" y="${y + 39}" text-anchor="middle" style="fill:${color.text}">${low.toFixed(2)}–${high.toFixed(2)}</text></g>`);
      } else if (isPlanned) {
        parts.push(`<g><title>Planned cell has no successful paired measurement</title><rect class="grid" x="${x}" y="${y}" width="${cellWidth}" height="${cellHeight}" fill="#e5e7eb"/><text class="unm" x="${x + cellWidth / 2}" y="${y + 31}" text-anchor="middle">UNMEASURED</text></g>`);
      } else {
        parts.push(`<g><title>Cell was not scheduled in the staged grid</title><rect class="grid" x="${x}" y="${y}" width="${cellWidth}" height="${cellHeight}" fill="url(#unscheduled)"/><text class="unm" x="${x + cellWidth / 2}" y="${y + 31}" text-anchor="middle">—</text></g>`);
      }
    });
  });
  parts.push(`<rect class="border" x="${gridX}" y="${gridY}" width="${rows.length * cellWidth}" height="${batches.length * cellHeight}"/>`);
}

const legendY = height - 42;
parts.push(`<rect x="${margin}" y="${legendY - 12}" width="22" height="16" fill="rgb(220,38,38)"/><text class="axis" x="${margin + 30}" y="${legendY}">&lt;1 full-query time is lower</text>`);
parts.push(`<rect x="${margin + 210}" y="${legendY - 12}" width="22" height="16" fill="rgb(229,231,235)"/><text class="axis" x="${margin + 240}" y="${legendY}">1 equal</text>`);
parts.push(`<rect x="${margin + 330}" y="${legendY - 12}" width="22" height="16" fill="rgb(37,99,235)"/><text class="axis" x="${margin + 360}" y="${legendY}">&gt;1 pg_ivm time is lower</text>`);
parts.push(`<rect x="${margin + 570}" y="${legendY - 12}" width="22" height="16" fill="#e5e7eb"/><text class="axis" x="${margin + 600}" y="${legendY}">UNMEASURED planned without a pair</text>`);
parts.push(`<rect x="${margin + 890}" y="${legendY - 12}" width="22" height="16" fill="url(#unscheduled)"/><text class="axis" x="${margin + 920}" y="${legendY}">— staged out</text>`);
parts.push(`</svg>`);

await writeFile(outputPath, `${parts.join("\n")}\n`);
console.log(JSON.stringify({ event: "crossover-heatmap", status: "ok", input: inputPath, output: outputPath, panels: panels.length, measured_cells: measured.size }));
