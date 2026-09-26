import { expect, test } from "vitest";
import { regions } from "./1_map";

test("every Rust region resolves and its TS mirror has the same number of lines", () => {
  const table = regions.map((region) => `${region.id.padEnd(12)} src/${region.file.padEnd(9)} ${region.anchor}`);
  expect(table.join("\n")).toMatchInlineSnapshot(`
    "rel          src/1_rel.rs  pub trait Rel {
    lower        src/1_rel.rs  pub fn lower<A: Rel>(p: &Program, a: &mut A)
    lower_node   src/1_rel.rs  pub fn lower_node<A: Rel>(
    dd_rel       src/2_dd.rs   impl<'s, T: Nest> Rel for DdRel<'s, T> {
    accumulable  src/2_dd.rs   fn accumulable<'s, T: Nest>(
    dd           src/2_dd.rs   impl Dd {
    dd_engine    src/2_dd.rs   impl Engine for Dd {
    worker       src/2_dd.rs   fn worker(program: Program
    count        src/2_dd.rs   fn count(trace: &mut Trace, row: &Row) -> W {
    read_trace   src/2_dd.rs   fn read_trace(trace: &mut Trace)
    guard        src/2_dd.rs   fn guard("
  `);
  for (const region of regions) {
    expect(region.ts_lines.length, region.id).toBe(region.rust_lines.length);
    expect(region.rust_lines[0], region.id).toContain(region.anchor);
  }
});
