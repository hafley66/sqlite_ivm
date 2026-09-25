import { useSyncExternalStore, type PointerEvent } from "react";

// One hover state for the whole page: the tokens of the concept under the pointer.
export const hover = {
  tokens: [] as string[],
  listeners: new Set<() => void>(),
  set(next: string[]) {
    if (next.join(" ") === hover.tokens.join(" ")) return;
    hover.tokens = next;
    hover.listeners.forEach((listener) => listener());
  },
  subscribe(listener: () => void) {
    hover.listeners.add(listener);
    return () => hover.listeners.delete(listener);
  },
};

export const useHover = () => useSyncExternalStore(hover.subscribe, () => hover.tokens);

const conceptTokens = (target: EventTarget | null) => {
  const element = target instanceof Element ? target.closest("[data-concept]") : null;
  return element?.getAttribute("data-concept")?.split(/\s+/).filter(Boolean) ?? [];
};

// Attach to the app root: onPointerOver / onPointerOut.
export const rootHoverHandlers = {
  onPointerOver: (event: PointerEvent) => hover.set(conceptTokens(event.target)),
  onPointerOut: (event: PointerEvent) => hover.set(conceptTokens(event.relatedTarget)),
};

export const quote = (token: string) => `"${token.replace(/["\\]/g, (match) => `\\${match}`)}"`;

// Column tokens match every row of the same shape, so they get a faint mark; every other token a strong one.
export const HoverStyle = () => {
  const hovered = useHover();
  const strong = hovered.filter((token) => !token.startsWith("column:"));
  const faint = hovered.filter((token) => token.startsWith("column:"));
  const rules = [
    strong.length
      ? `${strong.map((token) => `[data-concept~=${quote(token)}]`).join(",")}{background-color:rgb(250 204 21/.5);box-shadow:0 0 0 2px rgb(234 179 8/.8);border-radius:4px}`
      : "",
    faint.length
      ? `${faint.map((token) => `[data-concept~=${quote(token)}]`).join(",")}{outline:1px dashed rgb(234 179 8/.7)}`
      : "",
  ].join("\n");
  return <style>{rules}</style>;
};
