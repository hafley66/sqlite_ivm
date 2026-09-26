import { expect, test } from "vitest";
import { codeMap, rust, rustSources, ts } from "./1_map";

test("every concept has TS marker lines and one Rust line per anchor", () => {
  const table = Object.entries(codeMap).map(([token, concept]) => {
    const tsLines = [...ts.tagged.entries()].filter(([, tokens]) => tokens.includes(token)).map(([line]) => line);
    const files = concept.rust.map((excerpt) => `${excerpt.file}+${excerpt.lines}`);
    return `${token.padEnd(15)} ts ${tsLines.join(",").padEnd(6)} ${files.join(" ")}`;
  });
  expect(table.join("\n")).toMatchInlineSnapshot(`
    "code:build      ts 1,12   2_dd.rs+1 2_dd.rs+1 1_rel.rs+brace
    code:input      ts 2,3,4  2_dd.rs+3
    code:keyby      ts 6,7    2_dd.rs+2
    code:join       ts 7      2_dd.rs+brace
    code:map        ts 8      2_dd.rs+brace
    code:union      ts 10     2_dd.rs+brace
    code:distinct   ts 10     2_dd.rs+brace
    code:changes    ts 11     2_dd.rs+brace
    code:subscribe  ts 13     2_dd.rs+8
    code:update     ts 14,15  2_dd.rs+brace
    code:advance    ts 16     2_dd.rs+5
    code:step       ts 17     2_dd.rs+1 2_dd.rs+1"
  `);
  for (const excerpt of rust) {
    const first = rustSources[excerpt.file].split("\n")[excerpt.start - 1];
    const anchor = codeMap[excerpt.token].rust.find((candidate) => candidate.file === excerpt.file && first.includes(candidate.anchor));
    expect(anchor, `${excerpt.token} src/${excerpt.file}:${excerpt.start}`).not.toBeUndefined();
    expect(excerpt.end).toBeGreaterThanOrEqual(excerpt.start);
  }
});
