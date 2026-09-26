import { createHighlighterCore, type HighlighterCore, type ThemedToken } from "shiki/core";
import { createJavaScriptRegexEngine } from "shiki/engine/javascript";
import rustLang from "shiki/langs/rust.mjs";
import typescriptLang from "shiki/langs/typescript.mjs";
import githubLight from "shiki/themes/github-light.mjs";
import { useEffect, useState } from "react";
import { blockKey, regions, rowKey, type Region } from "../code/1_map";
import { quote, useHover } from "./2_hover";

const highlighter: Promise<HighlighterCore> = createHighlighterCore({
  themes: [githubLight],
  langs: [typescriptLang, rustLang],
  engine: createJavaScriptRegexEngine(),
});

type Painted = { ts: ThemedToken[][]; rust: ThemedToken[][] };

const paintAll = async (): Promise<Map<string, Painted>> => {
  const shiki = await highlighter;
  const tokens = (code: string[], lang: "typescript" | "rust") => shiki.codeToTokens(code.join("\n"), { lang, theme: "github-light" }).tokens;
  return new Map(regions.map((region) => [region.id, { ts: tokens(region.ts_lines, "typescript"), rust: tokens(region.rust_lines, "rust") }]));
};

const Line = ({ tokens, fallback }: { tokens: ThemedToken[] | undefined; fallback: string }) => (
  <code className="whitespace-pre-wrap break-all">
    {tokens ? tokens.map((token, index) => <span key={index} style={{ color: token.color }}>{token.content}</span>) : fallback}
  </code>
);

const tableCss = `
.mirror-row{display:grid;grid-template-columns:4.5ch minmax(0,1fr) 4.5ch minmax(0,1fr);font:11.5px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace;border-left:4px solid transparent}
.mirror-row>.n{color:#94a3b8;text-align:right;padding-right:1ch;user-select:none}
.mirror-row>.ts{padding-right:1.5ch;border-right:1px solid #e2e8f0}
.mirror-row>.rs{padding-left:1ch}
.mirror-row[data-concept]{box-shadow:none!important;border-radius:0!important}
`;

// The hovered block lights every row inside it (nested blocks included); the hovered row gets a bar and a darker fill.
const RowHoverStyle = ({ hovered }: { hovered: string[] }) => {
  const blocks = hovered.filter((token) => token.startsWith("code:blk:"));
  const rows = hovered.filter((token) => token.startsWith("code:row:"));
  return (
    <style>
      {[
        blocks.length ? `${blocks.map((token) => `.mirror-row[data-lit-by~=${quote(token)}]`).join(",")}{background-color:rgb(250 204 21/.45)}` : "",
        rows.length ? `${rows.map((token) => `.mirror-row[data-concept~=${quote(token)}]`).join(",")}{background-color:rgb(250 204 21/.8)!important;border-left-color:#a16207}` : "",
      ].join("\n")}
    </style>
  );
};

const RegionTable = ({ region, painted }: { region: Region; painted: Painted | undefined }) => (
  <section id={`region-${region.id}`} className="border-b border-slate-200">
    <div className="sticky top-0 z-10 grid grid-cols-2 border-b border-slate-200 bg-slate-50 font-mono text-[11px] text-slate-600">
      <div className="px-2 py-0.5">
        code/0_dd.mirror.ts:{region.ts_start}–{region.ts_start + region.ts_lines.length - 1}
      </div>
      <div className="px-2 py-0.5">
        <span className="font-semibold text-slate-800">{region.title}</span> src/{region.file}:{region.rust_start}–{region.rust_start + region.rust_lines.length - 1} ({region.rust_lines.length} lines)
      </div>
    </div>
    {region.rust_lines.map((rustLine, index) => (
      <div
        key={index}
        className="mirror-row"
        data-concept={`${blockKey(region.id, region.block_of[index])} ${rowKey(region.id, index)}`}
        data-lit-by={region.within[index].map((block) => blockKey(region.id, block)).join(" ")}
      >
        <span className="n">{region.ts_start + index}</span>
        <span className="ts"><Line tokens={painted?.ts[index]} fallback={region.ts_lines[index]} /></span>
        <span className="n">{region.rust_start + index}</span>
        <span className="rs"><Line tokens={painted?.rust[index]} fallback={rustLine} /></span>
      </div>
    ))}
  </section>
);

export const CodeView = () => {
  const [painted, setPainted] = useState<Map<string, Painted> | null>(null);
  const hovered = useHover();

  useEffect(() => {
    let live = true;
    paintAll().then((result) => live && setPainted(result));
    return () => {
      live = false;
    };
  }, []);

  const block = hovered.find((token) => token.startsWith("code:blk:"));
  const region = block ? regions.find((candidate) => block.startsWith(`code:blk:${candidate.id}:`)) : undefined;
  const caption = region && block ? region.captions.get(block) : undefined;

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      <style>{tableCss}</style>
      <RowHoverStyle hovered={hovered} />
      <nav className="flex flex-wrap items-center gap-1 text-xs">
        <span className="text-slate-500">Regions:</span>
        {regions.map((candidate) => (
          <button
            key={candidate.id}
            onClick={() => document.getElementById(`region-${candidate.id}`)?.scrollIntoView({ block: "start" })}
            className="rounded border border-slate-300 bg-white px-1.5 py-0.5 font-mono hover:bg-slate-100"
          >
            {candidate.title}
          </button>
        ))}
      </nav>
      <div className="flex min-h-0 flex-1 flex-col rounded-lg border border-slate-200 bg-white shadow-sm">
        <div className="grid grid-cols-2 border-b border-slate-200 text-xs font-semibold tracking-wide text-slate-500 uppercase">
          <h2 className="px-3 py-1.5">
            TypeScript mirror <span className="font-normal normal-case">(as if differential-dataflow were a TS library; code/0_dd.mirror.ts)</span>
          </h2>
          <h2 className="px-3 py-1.5">
            The engine's real Rust <span className="font-normal normal-case">(read from ../src at build time)</span>
          </h2>
        </div>
        <div className="min-h-0 flex-1 overflow-auto">
          {regions.map((candidate) => (
            <RegionTable key={candidate.id} region={candidate} painted={painted?.get(candidate.id)} />
          ))}
        </div>
      </div>
      <div className="min-h-10 rounded-lg border border-slate-200 bg-white px-3 py-2 text-sm shadow-sm">
        {region ? (
          <>
            <span className="font-mono font-semibold">{region.title}</span>: {caption ?? region.caption}
          </>
        ) : (
          <span className="text-slate-400 italic">Hover a row: it and its enclosing block light up on both sides.</span>
        )}
      </div>
    </div>
  );
};
