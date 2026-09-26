import { createHighlighterCore, type HighlighterCore, type ShikiTransformer } from "shiki/core";
import { createJavaScriptRegexEngine } from "shiki/engine/javascript";
import rustLang from "shiki/langs/rust.mjs";
import typescriptLang from "shiki/langs/typescript.mjs";
import githubLight from "shiki/themes/github-light.mjs";
import { useEffect, useRef, useState } from "react";
import { codeMap, rustBlocks, rustSources, rustTokensAt, ts } from "../code/1_map";
import { quote, useHover } from "./2_hover";

const highlighter: Promise<HighlighterCore> = createHighlighterCore({
  themes: [githubLight],
  langs: [typescriptLang, rustLang],
  engine: createJavaScriptRegexEngine(),
});

// Each rendered line carries its concept tokens and its real line number.
const tagLines = (tokensAt: (line: number) => string[], firstLine: number): ShikiTransformer => ({
  line(node, line) {
    const real = firstLine + line - 1;
    const tokens = tokensAt(real);
    node.properties["data-n"] = String(real);
    if (tokens.length) node.properties["data-concept"] = tokens.join(" ");
  },
});

type Rendered = { ts: string; rust: { title: string; html: string }[] };

const render = async (): Promise<Rendered> => {
  const shiki = await highlighter;
  return {
    ts: shiki.codeToHtml(ts.code, { lang: "typescript", theme: "github-light", transformers: [tagLines((line) => ts.tagged.get(line) ?? [], 1)] }),
    rust: rustBlocks.map((block) => ({
      title: `src/${block.file}  lines ${block.start}–${block.end}`,
      html: shiki.codeToHtml(rustSources[block.file].split("\n").slice(block.start - 1, block.end).join("\n"), {
        lang: "rust",
        theme: "github-light",
        transformers: [tagLines((line) => rustTokensAt(block.file, line), block.start)],
      }),
    })),
  };
};

const paneCss = `
.code-pane pre{margin:0;padding:6px 0;background:transparent!important;font-size:12px;line-height:1.55}
.code-pane .line{display:inline-block;width:100%;border-left:4px solid transparent;padding-right:8px}
.code-pane .line::before{content:attr(data-n);display:inline-block;width:4.5ch;margin-right:1.5ch;text-align:right;color:#94a3b8}
.code-pane .line[data-concept]{border-left-color:#e2e8f0;cursor:help}
.code-pane [data-concept]{box-shadow:none!important;border-radius:0!important}
`;

// Hovered code tokens: a strong left bar on every matching line, both panes.
const CodeHoverStyle = ({ hovered }: { hovered: string[] }) => {
  const code = hovered.filter((token) => token.startsWith("code:"));
  return code.length ? <style>{`${code.map((token) => `.code-pane .line[data-concept~=${quote(token)}]`).join(",")}{border-left-color:#ca8a04!important}`}</style> : null;
};

export const CodeView = () => {
  const [rendered, setRendered] = useState<Rendered | null>(null);
  const hovered = useHover();
  const panes = { ts: useRef<HTMLDivElement>(null), rust: useRef<HTMLDivElement>(null) };
  const activePane = useRef<"ts" | "rust" | null>(null);

  useEffect(() => {
    let live = true;
    render().then((result) => live && setRendered(result));
    return () => {
      live = false;
    };
  }, []);

  // Scroll the other pane to its first block for the hovered concept.
  useEffect(() => {
    const token = hovered.find((candidate) => candidate.startsWith("code:"));
    const other = activePane.current === "ts" ? panes.rust.current : activePane.current === "rust" ? panes.ts.current : null;
    const target = token && other?.querySelector<HTMLElement>(`.line[data-concept~=${quote(token)}]`);
    if (!other || !target) return;
    const top = target.getBoundingClientRect().top - other.getBoundingClientRect().top + other.scrollTop;
    if (top < other.scrollTop || top > other.scrollTop + other.clientHeight - 40) other.scrollTo({ top: top - other.clientHeight / 3, behavior: "smooth" });
  }, [hovered]);

  const captions = hovered.filter((token) => token in codeMap).map((token) => codeMap[token].caption);

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      <style>{paneCss}</style>
      <CodeHoverStyle hovered={hovered} />
      <div className="grid min-h-0 flex-1 grid-cols-2 gap-2">
        <section className="flex min-h-0 flex-col rounded-lg border border-slate-200 bg-white shadow-sm">
          <h2 className="border-b border-slate-200 px-3 py-1.5 text-xs font-semibold tracking-wide text-slate-500 uppercase">
            Imagined RxJS-style API for differential dataflow <span className="font-normal normal-case">(pseudo code, code/0_dd_like_rxjs.ts)</span>
          </h2>
          <div ref={panes.ts} onPointerEnter={() => (activePane.current = "ts")} className="code-pane min-h-0 flex-1 overflow-auto" dangerouslySetInnerHTML={{ __html: rendered?.ts ?? "" }} />
        </section>
        <section className="flex min-h-0 flex-col rounded-lg border border-slate-200 bg-white shadow-sm">
          <h2 className="border-b border-slate-200 px-3 py-1.5 text-xs font-semibold tracking-wide text-slate-500 uppercase">
            The engine's real Rust <span className="font-normal normal-case">(read from ../src at build time)</span>
          </h2>
          <div ref={panes.rust} onPointerEnter={() => (activePane.current = "rust")} className="code-pane min-h-0 flex-1 overflow-auto">
            {rendered?.rust.map((block) => (
              <div key={block.title} className="border-b border-slate-100">
                <div className="sticky top-0 z-10 bg-slate-50 px-3 py-0.5 font-mono text-[11px] text-slate-500">{block.title}</div>
                <div dangerouslySetInnerHTML={{ __html: block.html }} />
              </div>
            ))}
          </div>
        </section>
      </div>
      <div className="min-h-10 rounded-lg border border-slate-200 bg-white px-3 py-2 text-sm shadow-sm">
        {captions.length ? captions.map((caption) => <div key={caption}>{caption}</div>) : <span className="text-slate-400 italic">Hover a highlighted line on either side to see its partner and what it does.</span>}
      </div>
    </div>
  );
};
