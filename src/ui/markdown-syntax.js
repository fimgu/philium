// Marked extensions keep math out of the normal Markdown inline parser, so
// LaTeX escapes, underscores and asterisks reach the math renderer unchanged.
export const markdownExtensions = [
  {
    name: "displayMath",
    level: "block",
    start: (src) => src.match(/(?:^|\n)( {0,3})(?:\$\$|\\\[)/)?.index,
    tokenizer(src) {
      const match = /^ {0,3}\$\$([\s\S]+?)\$\$[ \t]*(?:\n|$)/.exec(src)
        || /^ {0,3}\\\[([\s\S]+?)\\\][ \t]*(?:\n|$)/.exec(src);
      if (match?.[1].trim()) return { type: "displayMath", raw: match[0], text: match[1].trim() };
    },
  },
  {
    name: "inlineMath",
    level: "inline",
    start: (src) => src.match(/\$|\\\(/)?.index,
    tokenizer(src) {
      // Do not interpret prose like "$5 or $10" as math.
      const match = /^\$(?![\s$])((?:\\[^\n]|[^\n$\\])+?)(?<!\s)\$(?![\d$])/.exec(src)
        || /^\\\(([^\n]+?)\\\)/.exec(src);
      if (match?.[1].trim()) return { type: "inlineMath", raw: match[0], text: match[1] };
    },
  },
  {
    name: "mark",
    level: "inline",
    start: (src) => src.indexOf("=="),
    tokenizer(src) {
      const match = /^==(?![=\s])([^\n]+?)(?<![=\s])==(?![=])/.exec(src);
      if (match) return { type: "mark", raw: match[0], tokens: this.lexer.inlineTokens(match[1]) };
    },
  },
];
