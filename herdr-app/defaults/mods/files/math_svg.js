// Pure-JS LaTeX math parser, 2D mathematical box layout engine, and SVG renderer.
import { escapeXml, svgToDataUrl, measureTextWidth, fmtNum } from "./svg_utils.js";

const MATH_SERIF = "Georgia, 'Times New Roman', 'Noto Serif', serif";
const MATH_SANS = "sans-serif";
const MATH_MONO = "monospace";

// Greek letters (lowercase italicized in TeX, uppercase upright)
const GREEK_LOWER = {
  alpha: "α", beta: "β", gamma: "γ", delta: "δ", epsilon: "ϵ", varepsilon: "ε",
  zeta: "ζ", eta: "η", theta: "θ", vartheta: "ϑ", iota: "ι", kappa: "κ", varkappa: "ϰ",
  lambda: "λ", mu: "μ", nu: "ν", xi: "ξ", omicron: "ο", pi: "π", varpi: "ϖ",
  rho: "ρ", varrho: "ϱ", sigma: "σ", varsigma: "ς", tau: "τ", upsilon: "υ",
  phi: "ϕ", varphi: "φ", chi: "χ", psi: "ψ", omega: "ω",
};

const GREEK_UPPER = {
  Gamma: "Γ", Delta: "Δ", Theta: "Θ", Lambda: "Λ", Xi: "Ξ",
  Pi: "Π", Sigma: "Σ", Upsilon: "Υ", Phi: "Φ", Psi: "Ψ", Omega: "Ω",
};

// Binary operators
const BIN_OPS = {
  pm: "±", mp: "∓", times: "×", div: "÷", cdot: "·", ast: "∗", star: "⋆",
  circ: "∘", bullet: "•", oplus: "⊕", ominus: "⊖", otimes: "⊗", oslash: "⊘", odot: "⊙",
  cup: "∪", cap: "∩", uplus: "⊎", sqcup: "⊔", sqcap: "⊓", setminus: "∖", smallsetminus: "∖",
  wedge: "∧", vee: "∨", land: "∧", lor: "∨", dagger: "†", ddagger: "‡",
  amalg: "⨿", wr: "≀", triangleleft: "◁", triangleright: "▷", bigcirc: "◯",
  diamond: "⋄", boxplus: "⊞", boxminus: "⊟", boxtimes: "⊠",
};

// Relation symbols
const REL_OPS = {
  le: "≤", leq: "≤", leqq: "≦", ge: "≥", geq: "≥", geqq: "≧",
  neq: "≠", ne: "≠", approx: "≈", equiv: "≡", sim: "∼", simeq: "≃", cong: "≅",
  propto: "∝", ll: "≪", gg: "≫", subset: "⊂", supset: "⊃", subseteq: "⊆", supseteq: "⊇",
  nsubseteq: "⊈", nsupseteq: "⊉", sqsubset: "⊏", sqsupset: "⊐", sqsubseteq: "⊑", sqsupseteq: "⊒",
  in: "∈", notin: "∉", ni: "∋", owns: "∋",
  perp: "⊥", parallel: "∥", mid: "∣", nmid: "∤", models: "⊨", vdash: "⊢", dashv: "⊣",
  prec: "≺", succ: "≻", preceq: "⪯", succeq: "⪰",
  triangleq: "≜", doteq: "≐", asymp: "≍", bowtie: "⋈", smile: "⌣", frown: "⌢",
  to: "→", rightarrow: "→", gets: "←", leftarrow: "←", leftrightarrow: "↔",
  Rightarrow: "⇒", Leftarrow: "⇐", Leftrightarrow: "⇔", iff: "⇔", implies: "⇒", impliedby: "⇐",
  mapsto: "↦", longmapsto: "⟼", longrightarrow: "⟶", longleftarrow: "⟵", longleftrightarrow: "⟷",
  Longrightarrow: "⟹", Longleftarrow: "⟸", Longleftrightarrow: "⟺",
  uparrow: "↑", downarrow: "↓", updownarrow: "↕", Uparrow: "⇑", Downarrow: "⇓", Updownarrow: "⇕",
  nearrow: "↗", searrow: "↘", swarrow: "↙", nwarrow: "↖",
  hookrightarrow: "↪", hookleftarrow: "↩", rightharpoonup: "⇀", rightharpoondown: "⇁",
  leftharpoonup: "↼", leftharpoondown: "↽", rightleftharpoons: "⇌",
  rightrightarrows: "⇉", leftleftarrows: "⇇", twoheadrightarrow: "↠",
  colon: ":",
};

// Miscellaneous ordinary symbols
const ORD_SYMS = {
  infty: "∞", partial: "∂", nabla: "∇", forall: "∀", exists: "∃", nexists: "∄",
  emptyset: "∅", varnothing: "∅", neg: "¬", lnot: "¬", ell: "ℓ", hbar: "ℏ", hslash: "ℏ",
  Re: "ℜ", Im: "ℑ", wp: "℘", aleph: "ℵ", beth: "ℶ", gimel: "ℷ",
  angle: "∠", measuredangle: "∡", sphericalangle: "∢",
  triangle: "△", triangledown: "▽", square: "□", Box: "□", blacksquare: "■",
  lozenge: "◊", blacklozenge: "◆", bigstar: "★",
  cdots: "⋯", ldots: "…", dots: "…", dotsc: "…", dotsb: "⋯", dotsm: "⋯", dotsi: "⋯", dotso: "…",
  vdots: "⋮", ddots: "⋱", prime: "′", backprime: "‵",
  degree: "°", top: "⊤", bot: "⊥", flat: "♭", natural: "♮", sharp: "♯",
  checkmark: "✓", clubsuit: "♣", diamondsuit: "♢", heartsuit: "♡", spadesuit: "♠",
  imath: "ı", jmath: "ȷ", eth: "ð", mho: "℧", S: "§", P: "¶", pounds: "£",
  backslash: "\\", vert: "|", Vert: "‖", lvert: "|", rvert: "|", lVert: "‖", rVert: "‖",
  langle: "⟨", rangle: "⟩", lfloor: "⌊", rfloor: "⌋", lceil: "⌈", rceil: "⌉",
  lbrace: "{", rbrace: "}", lbrack: "[", rbrack: "]",
  quad: "\u2003", qquad: "\u2003\u2003",
};

// Large n-ary operators (true => limits above/below in display mode)
const BIG_OPS = {
  sum: { ch: "∑", limits: true },
  prod: { ch: "∏", limits: true },
  coprod: { ch: "∐", limits: true },
  int: { ch: "∫", limits: false },
  iint: { ch: "∬", limits: false },
  iiint: { ch: "∭", limits: false },
  oint: { ch: "∮", limits: false },
  oiint: { ch: "∯", limits: false },
  bigcup: { ch: "⋃", limits: true },
  bigcap: { ch: "⋂", limits: true },
  bigsqcup: { ch: "⨆", limits: true },
  bigvee: { ch: "⋁", limits: true },
  bigwedge: { ch: "⋀", limits: true },
  bigoplus: { ch: "⨁", limits: true },
  bigotimes: { ch: "⨂", limits: true },
  bigodot: { ch: "⨀", limits: true },
  biguplus: { ch: "⨄", limits: true },
};

// Upright named math functions (true => display-mode limits below/above)
const NAMED_FUNCS = {
  sin: false, cos: false, tan: false, cot: false, sec: false, csc: false,
  arcsin: false, arccos: false, arctan: false, sinh: false, cosh: false, tanh: false, coth: false,
  log: false, ln: false, lg: false, exp: false,
  det: true, dim: false, ker: false, hom: false, arg: false, deg: false, gcd: true, lcm: false,
  Pr: true, mod: false, bmod: false, rank: false, tr: false, span: false, diag: false,
  sgn: false, sign: false, var: false, Var: false, Cov: false, Corr: false,
  lim: true, limsup: true, liminf: true, sup: true, inf: true, max: true, min: true,
  argmax: true, argmin: true,
};

const BLACKBOARD = {
  A: "𝔸", B: "𝔹", C: "ℂ", D: "𝔻", E: "𝔼", F: "𝔽", G: "𝔾", H: "ℍ", I: "𝕀", J: "𝕁", K: "𝕂", L: "𝕃", M: "𝕄",
  N: "ℕ", O: "𝕆", P: "ℙ", Q: "ℚ", R: "ℝ", S: "𝕊", T: "𝕋", U: "𝕌", V: "𝕍", W: "𝕎", X: "𝕏", Y: "𝕐", Z: "ℤ",
  k: "𝕜", 0: "𝟘", 1: "𝟙", 2: "𝟚",
};

const CALLIGRAPHIC = {
  A: "𝒜", B: "ℬ", C: "𝒞", D: "𝒟", E: "ℰ", F: "ℱ", G: "𝒢", H: "ℋ", I: "ℐ", J: "𝒥", K: "𝒦", L: "ℒ", M: "ℳ",
  N: "𝒩", O: "𝒪", P: "𝒫", Q: "𝒬", R: "ℛ", S: "𝒮", T: "𝒯", U: "𝒰", V: "𝒱", W: "𝒲", X: "𝒳", Y: "𝒴", Z: "𝒵",
};

const FRAKTUR = {
  A: "𝔄", B: "𝔅", C: "ℭ", D: "𝔇", E: "𝔈", F: "𝔉", G: "𝔊", H: "ℌ", I: "ℑ", J: "𝔍", K: "𝔎", L: "𝔏", M: "𝔐",
  N: "𝔑", O: "𝔒", P: "𝔓", Q: "𝔔", R: "ℜ", S: "𝔖", T: "𝔗", U: "𝔘", V: "𝔙", W: "𝔚", X: "𝔛", Y: "𝔜", Z: "ℨ",
  a: "𝔞", b: "𝔟", c: "𝔠", d: "𝔡", e: "𝔢", f: "𝔣", g: "𝔤", h: "𝔥", i: "𝔦", j: "𝔧", k: "𝔨", l: "𝔩", m: "𝔪",
  n: "𝔫", o: "𝔬", p: "𝔭", q: "𝔮", r: "𝔯", s: "𝔰", t: "𝔱", u: "𝔲", v: "𝔳", w: "𝔴", x: "𝔵", y: "𝔶", z: "𝔷",
};

const ACCENT_CMDS = new Set([
  "hat", "widehat", "bar", "overline", "underline", "vec", "overrightarrow", "overleftarrow",
  "tilde", "widetilde", "dot", "ddot", "dddot", "acute", "grave", "check", "breve",
  "overbrace", "underbrace", "cancel",
]);

const SPACING_CMDS = {
  ",": 0.17, ":": 0.22, ">": 0.22, ";": 0.28, "!": -0.14, " ": 0.32,
  thinspace: 0.17, medspace: 0.22, thickspace: 0.28, enspace: 0.5,
  quad: 1.0, qquad: 2.0, negthinspace: -0.17,
};

const BIG_DELIM_SCALES = {
  big: 1.2, bigl: 1.2, bigr: 1.2, bigm: 1.2,
  Big: 1.5, Bigl: 1.5, Bigr: 1.5, Bigm: 1.5,
  bigg: 1.85, biggl: 1.85, biggr: 1.85, biggm: 1.85,
  Bigg: 2.2, Biggl: 2.2, Biggr: 2.2, Biggm: 2.2,
};

const TEXT_STYLE_CMDS = new Set([
  "text", "textrm", "textbf", "textit", "texttt", "textsf", "hbox", "mbox",
  "mathrm", "mathbf", "mathit", "mathsf", "mathtt", "mathbb", "mathcal",
  "mathfrak", "mathscr", "boldsymbol", "bm", "operatorname",
]);

const DELIM_MAP = {
  "(": "(", ")": ")", "[": "[", "]": "]",
  "{": "{", "}": "}", "\\{": "{", "\\}": "}",
  "|": "|", "\\|": "‖", "\\vert": "|", "\\Vert": "‖", "\\lvert": "|", "\\rvert": "|", "\\lVert": "‖", "\\rVert": "‖",
  "\\langle": "⟨", "\\rangle": "⟩", "<": "⟨", ">": "⟩",
  "\\lfloor": "⌊", "\\rfloor": "⌋", "\\lceil": "⌈", "\\rceil": "⌉",
  "\\lbrace": "{", "\\rbrace": "}", "\\lbrack": "[", "\\rbrack": "]",
  "/": "/", "\\backslash": "\\", ".": "",
};

const MATRIX_DELIMS = {
  matrix: ["", ""],
  smallmatrix: ["", ""],
  pmatrix: ["(", ")"],
  bmatrix: ["[", "]"],
  Bmatrix: ["{", "}"],
  vmatrix: ["|", "|"],
  Vmatrix: ["‖", "‖"],
  cases: ["{", ""],
  dcases: ["{", ""],
  rcases: ["", "}"],
  aligned: ["", ""],
  align: ["", ""],
  "align*": ["", ""],
  alignedat: ["", ""],
  gather: ["", ""],
  "gather*": ["", ""],
  gathered: ["", ""],
  split: ["", ""],
  equation: ["", ""],
  "equation*": ["", ""],
  eqnarray: ["", ""],
  "eqnarray*": ["", ""],
  array: ["", ""],
  substack: ["", ""],
};

// ---------------- Tokenizer ----------------

function tokenizeMath(src) {
  const s = String(src ?? "");
  const tokens = [];
  let i = 0;
  while (i < s.length) {
    const ch = s[i];
    if (ch === "%") {
      while (i < s.length && s[i] !== "\n") i++;
      continue;
    }
    if (/\s/.test(ch)) {
      i++;
      continue;
    }
    if (ch === "\\") {
      if (i + 1 >= s.length) {
        tokens.push({ type: "char", value: "\\" });
        i++;
        continue;
      }
      const next = s[i + 1];
      if (/[a-zA-Z]/.test(next)) {
        let j = i + 1;
        while (j < s.length && /[a-zA-Z]/.test(s[j])) j++;
        // Support starred commands like \operatorname*
        if (j < s.length && s[j] === "*" && (s.slice(i + 1, j) === "operatorname" || s.slice(i + 1, j) === "hspace")) {
          j++;
        }
        tokens.push({ type: "cmd", value: s.slice(i + 1, j) });
        i = j;
      } else {
        tokens.push({ type: "cmd", value: next });
        i += 2;
      }
      continue;
    }
    if (ch === "{" || ch === "}" || ch === "^" || ch === "_" || ch === "&" || ch === "'" || ch === "[" || ch === "]") {
      tokens.push({ type: ch, value: ch });
      i++;
      continue;
    }
    if (ch === "~") {
      tokens.push({ type: "cmd", value: " " });
      i++;
      continue;
    }
    // Consume multi-digit numbers together for clean kerning
    if (/[0-9]/.test(ch)) {
      let j = i + 1;
      while (j < s.length && /[0-9]/.test(s[j])) j++;
      tokens.push({ type: "num", value: s.slice(i, j) });
      i = j;
      continue;
    }
    tokens.push({ type: "char", value: ch });
    i++;
  }
  return tokens;
}

// ---------------- Parser ----------------

class MathParser {
  constructor(tokens) {
    this.tokens = tokens;
    this.pos = 0;
  }

  peek(offset = 0) {
    return this.tokens[this.pos + offset] ?? null;
  }

  next() {
    return this.tokens[this.pos++] ?? null;
  }

  eof() {
    return this.pos >= this.tokens.length;
  }

  parseSequence(stopPred = null) {
    const items = [];
    while (!this.eof()) {
      const tok = this.peek();
      if (stopPred && stopPred(tok)) break;
      if (tok.type === "}") break;
      if (tok.type === "cmd" && (tok.value === "over" || tok.value === "choose" || tok.value === "atop")) {
        this.next();
        const right = this.parseSequence(stopPred);
        const leftNode = items.length === 1 ? items[0] : { type: "row", items: items.slice() };
        const rightNode = right.length === 1 ? right[0] : { type: "row", items: right };
        const fracNode = {
          type: "frac",
          num: leftNode,
          den: rightNode,
          lineThickness: tok.value === "over" ? 1 : 0,
          leftDelim: tok.value === "choose" ? "(" : "",
          rightDelim: tok.value === "choose" ? ")" : "",
        };
        return [fracNode];
      }
      const atom = this.parseAtomWithScripts();
      if (atom) items.push(atom);
    }
    return items;
  }

  parseGroupOrAtom() {
    const tok = this.peek();
    if (!tok) return { type: "row", items: [] };
    if (tok.type === "{") {
      this.next();
      const items = this.parseSequence((t) => t.type === "}");
      if (this.peek()?.type === "}") this.next();
      return items.length === 1 ? items[0] : { type: "row", items };
    }
    return this.parseAtom() ?? { type: "row", items: [] };
  }

  parseOptionalBracket() {
    if (this.peek()?.type !== "[") return null;
    this.next();
    const items = this.parseSequence((t) => t.type === "]");
    if (this.peek()?.type === "]") this.next();
    return items.length === 1 ? items[0] : { type: "row", items };
  }

  parseRawTextGroup() {
    const tok = this.peek();
    if (!tok) return "";
    if (tok.type === "{") {
      this.next();
      let out = "";
      let depth = 1;
      while (!this.eof() && depth > 0) {
        const t = this.next();
        if (t.type === "{") { depth++; out += "{"; }
        else if (t.type === "}") { depth--; if (depth > 0) out += "}"; }
        else if (t.type === "cmd") {
          if (t.value === " " || t.value === "," || t.value === ";") out += " ";
          else if (GREEK_LOWER[t.value]) out += GREEK_LOWER[t.value];
          else if (GREEK_UPPER[t.value]) out += GREEK_UPPER[t.value];
          else if (ORD_SYMS[t.value]) out += ORD_SYMS[t.value];
          else if (REL_OPS[t.value]) out += REL_OPS[t.value];
          else if (BIN_OPS[t.value]) out += BIN_OPS[t.value];
          else if (t.value.length === 1) out += t.value;
          else out += t.value;
        } else {
          out += t.value;
        }
      }
      return out;
    }
    const t = this.next();
    return t ? t.value : "";
  }

  parseDelimiterToken() {
    const tok = this.next();
    if (!tok) return "";
    const key = tok.type === "cmd" ? `\\${tok.value}` : tok.value;
    if (key in DELIM_MAP) return DELIM_MAP[key];
    return tok.value;
  }

  parseAtomWithScripts() {
    let base = this.parseAtom();
    if (!base) return null;

    let sub = null;
    let sup = null;
    let limitsOverride = null;
    let primes = 0;

    while (!this.eof()) {
      const tok = this.peek();
      if (tok.type === "'") {
        this.next();
        primes++;
        continue;
      }
      if (tok.type === "cmd" && (tok.value === "limits" || tok.value === "nolimits")) {
        this.next();
        limitsOverride = tok.value === "limits";
        continue;
      }
      if (tok.type === "_") {
        this.next();
        sub = this.parseGroupOrAtom();
        continue;
      }
      if (tok.type === "^") {
        this.next();
        sup = this.parseGroupOrAtom();
        continue;
      }
      break;
    }

    if (primes > 0) {
      const primeNode = { type: "symbol", text: "′".repeat(primes), atom: "ord", upright: true };
      sup = sup ? { type: "row", items: [primeNode, sup] } : primeNode;
    }

    if (sub || sup) {
      const defaultLimits = Boolean(base.limits);
      return {
        type: "script",
        base,
        sub,
        sup,
        limits: limitsOverride !== null ? limitsOverride : defaultLimits,
      };
    }
    return base;
  }

  parseAtom() {
    const tok = this.peek();
    if (!tok) return null;

    if (tok.type === "{") {
      this.next();
      const items = this.parseSequence((t) => t.type === "}");
      if (this.peek()?.type === "}") this.next();
      return { type: "row", items };
    }

    if (tok.type === "}") {
      this.next();
      return null;
    }

    if (tok.type === "^" || tok.type === "_") {
      // Bare script with empty base
      return { type: "symbol", text: "", atom: "ord", upright: true };
    }

    if (tok.type === "'") {
      let count = 0;
      while (this.peek()?.type === "'") { this.next(); count++; }
      return { type: "symbol", text: "′".repeat(count), atom: "ord", upright: true };
    }

    if (tok.type === "num") {
      this.next();
      return { type: "symbol", text: tok.value, atom: "ord", upright: true };
    }

    if (tok.type === "[" || tok.type === "]") {
      this.next();
      return { type: "symbol", text: tok.value, atom: tok.type === "[" ? "open" : "close", upright: true };
    }

    if (tok.type === "char") {
      this.next();
      const ch = tok.value;
      if (ch === "+" || ch === "-") {
        return { type: "symbol", text: ch === "-" ? "−" : "+", atom: "bin", upright: true };
      }
      if (ch === "*") {
        return { type: "symbol", text: "∗", atom: "bin", upright: true };
      }
      if (ch === "=" || ch === "<" || ch === ">") {
        return { type: "symbol", text: ch, atom: "rel", upright: true };
      }
      if (ch === "," || ch === ";") {
        return { type: "symbol", text: ch, atom: "punct", upright: true };
      }
      if (ch === ":") {
        return { type: "symbol", text: ":", atom: "rel", upright: true };
      }
      if (ch === "(") return { type: "symbol", text: "(", atom: "open", upright: true };
      if (ch === ")") return { type: "symbol", text: ")", atom: "close", upright: true };
      if (/[a-zA-Z]/.test(ch)) {
        return { type: "symbol", text: ch, atom: "ord", italic: true };
      }
      return { type: "symbol", text: ch, atom: "ord", upright: true };
    }

    if (tok.type === "&") {
      this.next();
      return { type: "align_tab" };
    }

    if (tok.type === "cmd") {
      return this.parseCommand();
    }

    this.next();
    return null;
  }

  parseCommand() {
    const tok = this.next();
    const cmd = tok.value;

    // Line break inside top-level or environments
    if (cmd === "\\") {
      if (this.peek()?.type === "[") this.parseOptionalBracket();
      return { type: "newline" };
    }

    // Escaped literals
    if (cmd === "{" || cmd === "}" || cmd === "%" || cmd === "$" || cmd === "&" || cmd === "#" || cmd === "_") {
      return { type: "symbol", text: cmd, atom: cmd === "{" ? "open" : cmd === "}" ? "close" : "ord", upright: true };
    }
    if (cmd === "|") {
      return { type: "symbol", text: "‖", atom: "ord", upright: true };
    }

    // Spacing
    if (cmd in SPACING_CMDS) {
      return { type: "space", em: SPACING_CMDS[cmd] };
    }
    if (cmd === "hspace" || cmd === "hspace*") {
      this.parseRawTextGroup();
      return { type: "space", em: 0.6 };
    }
    if (cmd === "qquad") return { type: "space", em: 2.0 };
    if (cmd === "quad") return { type: "space", em: 1.0 };

    // Style switches (softly supported)
    if (cmd === "displaystyle" || cmd === "textstyle" || cmd === "scriptstyle" || cmd === "scriptscriptstyle") {
      return { type: "style", mode: cmd };
    }

    // Fractions & binomials
    if (cmd === "frac" || cmd === "dfrac" || cmd === "tfrac" || cmd === "cfrac") {
      const num = this.parseGroupOrAtom();
      const den = this.parseGroupOrAtom();
      return {
        type: "frac",
        num,
        den,
        lineThickness: 1,
        forceDisplay: cmd === "dfrac" || cmd === "cfrac",
        forceText: cmd === "tfrac",
        leftDelim: "",
        rightDelim: "",
      };
    }
    if (cmd === "binom" || cmd === "dbinom" || cmd === "tbinom") {
      const num = this.parseGroupOrAtom();
      const den = this.parseGroupOrAtom();
      return {
        type: "frac",
        num,
        den,
        lineThickness: 0,
        forceDisplay: cmd === "dbinom",
        forceText: cmd === "tbinom",
        leftDelim: "(",
        rightDelim: ")",
      };
    }

    // Roots
    if (cmd === "sqrt") {
      const index = this.parseOptionalBracket();
      const radicand = this.parseGroupOrAtom();
      return { type: "sqrt", radicand, index };
    }

    // Stretchy delimiters \left ... \right
    if (cmd === "left") {
      const leftDelim = this.parseDelimiterToken();
      const items = this.parseSequence((t) => t.type === "cmd" && t.value === "right");
      let rightDelim = "";
      if (this.peek()?.type === "cmd" && this.peek().value === "right") {
        this.next();
        rightDelim = this.parseDelimiterToken();
      }
      return {
        type: "delim",
        left: leftDelim,
        right: rightDelim,
        body: items.length === 1 ? items[0] : { type: "row", items },
      };
    }
    if (cmd === "middle") {
      const d = this.parseDelimiterToken();
      return { type: "symbol", text: d || "|", atom: "rel", upright: true };
    }
    if (cmd in BIG_DELIM_SCALES) {
      const d = this.parseDelimiterToken();
      return { type: "big_delim", delim: d, scale: BIG_DELIM_SCALES[cmd] };
    }

    // Environments \begin{...} ... \end{...}
    if (cmd === "begin") {
      const envName = this.parseRawTextGroup().trim();
      let colSpec = "";
      if (envName === "array" && this.peek()?.type === "{") {
        colSpec = this.parseRawTextGroup().trim();
      } else if (envName === "alignedat" && this.peek()?.type === "{") {
        this.parseRawTextGroup();
      }
      const rows = this.parseEnvironmentRows(envName);
      return { type: "matrix", env: envName, rows, colSpec };
    }
    if (cmd === "end") {
      this.parseRawTextGroup();
      return null;
    }

    // \substack{... \\ ...}
    if (cmd === "substack") {
      if (this.peek()?.type === "{") {
        this.next();
        const rows = [];
        let cur = [];
        while (!this.eof() && this.peek()?.type !== "}") {
          const item = this.parseAtomWithScripts();
          if (!item) continue;
          if (item.type === "newline") {
            rows.push([{ type: "row", items: cur }]);
            cur = [];
          } else {
            cur.push(item);
          }
        }
        if (this.peek()?.type === "}") this.next();
        if (cur.length) rows.push([{ type: "row", items: cur }]);
        return { type: "matrix", env: "substack", rows, colSpec: "c" };
      }
    }

    // Accents & over/under decorations
    if (ACCENT_CMDS.has(cmd)) {
      const base = this.parseGroupOrAtom();
      return { type: "accent", kind: cmd, base, limits: cmd === "overbrace" || cmd === "underbrace" };
    }

    // Stacked relations / overset / underset / stackrel
    if (cmd === "overset" || cmd === "stackrel") {
      const over = this.parseGroupOrAtom();
      const base = this.parseGroupOrAtom();
      return { type: "stack", base, over, under: null };
    }
    if (cmd === "underset") {
      const under = this.parseGroupOrAtom();
      const base = this.parseGroupOrAtom();
      return { type: "stack", base, over: null, under };
    }

    // Boxes & color
    if (cmd === "boxed" || cmd === "fbox") {
      const body = this.parseGroupOrAtom();
      return { type: "boxed", body };
    }
    if (cmd === "phantom" || cmd === "hphantom" || cmd === "vphantom") {
      const body = this.parseGroupOrAtom();
      return { type: "phantom", body };
    }
    if (cmd === "textcolor" || cmd === "color") {
      const color = this.parseRawTextGroup().trim();
      const body = this.parseGroupOrAtom();
      return { type: "color", color, body };
    }
    if (cmd === "not") {
      const target = this.parseAtom();
      if (target?.type === "symbol") {
        const negated = { "=": "≠", "∈": "∉", "⊂": "⊄", "⊆": "⊈", "≡": "≢", "≈": "≉", "<": "≮", ">": "≯", "≤": "≰", "≥": "≱", "∼": "≁", "∣": "∤" }[target.text];
        if (negated) return { ...target, text: negated };
      }
      return target ? { type: "accent", kind: "cancel", base: target } : null;
    }

    // \pmod{n}
    if (cmd === "pmod") {
      const arg = this.parseGroupOrAtom();
      return {
        type: "row",
        items: [
          { type: "space", em: 0.45 },
          { type: "symbol", text: "(", atom: "open", upright: true },
          { type: "symbol", text: "mod", atom: "op", upright: true },
          { type: "space", em: 0.25 },
          arg,
          { type: "symbol", text: ")", atom: "close", upright: true },
        ],
      };
    }

    // Text & math alphabets
    if (TEXT_STYLE_CMDS.has(cmd) || cmd === "operatorname*") {
      const isStarOp = cmd === "operatorname*";
      if (cmd === "text" || cmd === "textrm" || cmd === "textbf" || cmd === "textit" || cmd === "texttt" || cmd === "textsf" || cmd === "hbox" || cmd === "mbox" || cmd === "operatorname" || isStarOp) {
        const text = this.parseRawTextGroup();
        return {
          type: "symbol",
          text,
          atom: cmd === "operatorname" || isStarOp ? "op" : "ord",
          upright: cmd !== "textit",
          italic: cmd === "textit",
          bold: cmd === "textbf",
          mono: cmd === "texttt",
          sans: cmd === "textsf",
          limits: isStarOp,
        };
      }
      if (cmd === "mathbb" || cmd === "mathcal" || cmd === "mathfrak" || cmd === "mathscr") {
        const raw = this.parseRawTextGroup();
        const table = cmd === "mathbb" ? BLACKBOARD : cmd === "mathfrak" ? FRAKTUR : CALLIGRAPHIC;
        let mapped = "";
        for (const ch of raw) mapped += table[ch] ?? ch;
        return { type: "symbol", text: mapped, atom: "ord", upright: true };
      }
      // \mathrm, \mathbf, \mathit, \mathsf, \mathtt, \boldsymbol, \bm
      const body = this.parseGroupOrAtom();
      return {
        type: "font",
        font: cmd,
        body,
      };
    }

    // Big operators
    if (cmd in BIG_OPS) {
      const op = BIG_OPS[cmd];
      return { type: "bigop", text: op.ch, limits: op.limits };
    }

    // Named functions
    if (cmd in NAMED_FUNCS) {
      const label = cmd === "limsup" ? "lim sup" : cmd === "liminf" ? "lim inf" : cmd === "argmax" ? "arg max" : cmd === "argmin" ? "arg min" : cmd === "bmod" ? "mod" : cmd;
      return { type: "symbol", text: label, atom: "op", upright: true, limits: NAMED_FUNCS[cmd] };
    }

    // Greek & symbols
    if (cmd in GREEK_LOWER) {
      return { type: "symbol", text: GREEK_LOWER[cmd], atom: "ord", italic: true };
    }
    if (cmd in GREEK_UPPER) {
      return { type: "symbol", text: GREEK_UPPER[cmd], atom: "ord", upright: true };
    }
    if (cmd in BIN_OPS) {
      return { type: "symbol", text: BIN_OPS[cmd], atom: "bin", upright: true };
    }
    if (cmd in REL_OPS) {
      return { type: "symbol", text: REL_OPS[cmd], atom: "rel", upright: true };
    }
    if (cmd in ORD_SYMS) {
      const atom = ["⟨", "⌊", "⌈", "{", "["].includes(ORD_SYMS[cmd])
        ? "open"
        : ["⟩", "⌋", "⌉", "}", "]"].includes(ORD_SYMS[cmd])
          ? "close"
          : "ord";
      return { type: "symbol", text: ORD_SYMS[cmd], atom, upright: true };
    }

    // Ignore layout-only commands like \label{...}, \tag{...}, \nonumber, \notag
    if (cmd === "label" || cmd === "ref" || cmd === "eqref") {
      const v = this.parseRawTextGroup();
      return cmd === "label" ? null : { type: "symbol", text: cmd === "eqref" ? `(${v})` : v, atom: "ord", upright: true };
    }
    if (cmd === "tag") {
      const v = this.parseRawTextGroup();
      return { type: "row", items: [{ type: "space", em: 1.2 }, { type: "symbol", text: `(${v})`, atom: "ord", upright: true }] };
    }
    if (cmd === "nonumber" || cmd === "notag" || cmd === "hline" || cmd === "cr") {
      return cmd === "cr" ? { type: "newline" } : null;
    }

    // Unknown command fallback: render cleanly without crashing
    return { type: "symbol", text: cmd, atom: "ord", upright: true };
  }

  parseEnvironmentRows(envName) {
    const rows = [];
    let row = [];
    let cell = [];
    const flushCell = () => {
      row.push(cell.length === 1 ? cell[0] : { type: "row", items: cell });
      cell = [];
    };
    const flushRow = () => {
      flushCell();
      rows.push(row);
      row = [];
    };
    while (!this.eof()) {
      const tok = this.peek();
      if (tok.type === "cmd" && tok.value === "end") {
        this.next();
        this.parseRawTextGroup();
        break;
      }
      if (tok.type === "&") {
        this.next();
        flushCell();
        continue;
      }
      const item = this.parseAtomWithScripts();
      if (!item) continue;
      if (item.type === "newline") {
        flushRow();
      } else {
        cell.push(item);
      }
    }
    if (cell.length > 0 || row.length > 0) flushRow();
    return rows;
  }
}

export function parseMathAst(latex) {
  const tokens = tokenizeMath(latex);
  const parser = new MathParser(tokens);
  const items = parser.parseSequence();
  // Split top-level newlines (`\\`) into stacked rows automatically!
  const lines = [];
  let cur = [];
  for (const item of items) {
    if (item.type === "newline") {
      lines.push(cur);
      cur = [];
    } else {
      cur.push(item);
    }
  }
  lines.push(cur);
  if (lines.length > 1) {
    return {
      type: "matrix",
      env: "gather",
      rows: lines.map((line) => [line.length === 1 ? line[0] : { type: "row", items: line }]),
      colSpec: "c",
    };
  }
  return items.length === 1 ? items[0] : { type: "row", items };
}

// ---------------- 2D Box Layout Engine ----------------

function makeEmptyBox(fontSize) {
  return { w: 0, ascent: fontSize * 0.72, descent: fontSize * 0.2, axis: fontSize * 0.28, atom: "ord", svg: () => "" };
}

function drawStretchyDelim(delim, x, topY, bottomY, fontSize, color) {
  if (!delim) return { w: 0, svg: "" };
  const h = Math.max(fontSize * 1.05, bottomY - topY);
  const midY = (topY + bottomY) / 2;
  const sw = Math.max(1.05, Math.min(1.8, fontSize * 0.08));
  const stroke = `stroke="${escapeXml(color)}" stroke-width="${fmtNum(sw)}" fill="none" stroke-linecap="round" stroke-linejoin="round"`;

  if (delim === "(") {
    const w = Math.max(6, Math.min(11, fontSize * 0.42 + h * 0.04));
    const cx = x + w * 0.78;
    const bx = x + w * 0.16;
    const d = `M ${fmtNum(cx)} ${fmtNum(topY)} Q ${fmtNum(bx)} ${fmtNum(midY)} ${fmtNum(cx)} ${fmtNum(bottomY)}`;
    return { w, svg: `<path d="${d}" ${stroke}/>` };
  }
  if (delim === ")") {
    const w = Math.max(6, Math.min(11, fontSize * 0.42 + h * 0.04));
    const cx = x + w * 0.22;
    const bx = x + w * 0.84;
    const d = `M ${fmtNum(cx)} ${fmtNum(topY)} Q ${fmtNum(bx)} ${fmtNum(midY)} ${fmtNum(cx)} ${fmtNum(bottomY)}`;
    return { w, svg: `<path d="${d}" ${stroke}/>` };
  }
  if (delim === "[") {
    const w = Math.max(6, Math.min(10, fontSize * 0.4));
    const x1 = x + w * 0.25;
    const x2 = x + w * 0.85;
    const d = `M ${fmtNum(x2)} ${fmtNum(topY)} L ${fmtNum(x1)} ${fmtNum(topY)} L ${fmtNum(x1)} ${fmtNum(bottomY)} L ${fmtNum(x2)} ${fmtNum(bottomY)}`;
    return { w, svg: `<path d="${d}" ${stroke}/>` };
  }
  if (delim === "]") {
    const w = Math.max(6, Math.min(10, fontSize * 0.4));
    const x1 = x + w * 0.15;
    const x2 = x + w * 0.75;
    const d = `M ${fmtNum(x1)} ${fmtNum(topY)} L ${fmtNum(x2)} ${fmtNum(topY)} L ${fmtNum(x2)} ${fmtNum(bottomY)} L ${fmtNum(x1)} ${fmtNum(bottomY)}`;
    return { w, svg: `<path d="${d}" ${stroke}/>` };
  }
  if (delim === "{") {
    const w = Math.max(8, Math.min(13, fontSize * 0.52 + h * 0.03));
    const r = Math.min(w * 0.38, h * 0.18);
    const xr = x + w * 0.84;
    const xm = x + w * 0.48;
    const xl = x + w * 0.12;
    const d = [
      `M ${fmtNum(xr)} ${fmtNum(topY)}`,
      `Q ${fmtNum(xm)} ${fmtNum(topY)} ${fmtNum(xm)} ${fmtNum(topY + r)}`,
      `L ${fmtNum(xm)} ${fmtNum(midY - r)}`,
      `Q ${fmtNum(xm)} ${fmtNum(midY)} ${fmtNum(xl)} ${fmtNum(midY)}`,
      `Q ${fmtNum(xm)} ${fmtNum(midY)} ${fmtNum(xm)} ${fmtNum(midY + r)}`,
      `L ${fmtNum(xm)} ${fmtNum(bottomY - r)}`,
      `Q ${fmtNum(xm)} ${fmtNum(bottomY)} ${fmtNum(xr)} ${fmtNum(bottomY)}`,
    ].join(" ");
    return { w, svg: `<path d="${d}" ${stroke}/>` };
  }
  if (delim === "}") {
    const w = Math.max(8, Math.min(13, fontSize * 0.52 + h * 0.03));
    const r = Math.min(w * 0.38, h * 0.18);
    const xl = x + w * 0.16;
    const xm = x + w * 0.52;
    const xr = x + w * 0.88;
    const d = [
      `M ${fmtNum(xl)} ${fmtNum(topY)}`,
      `Q ${fmtNum(xm)} ${fmtNum(topY)} ${fmtNum(xm)} ${fmtNum(topY + r)}`,
      `L ${fmtNum(xm)} ${fmtNum(midY - r)}`,
      `Q ${fmtNum(xm)} ${fmtNum(midY)} ${fmtNum(xr)} ${fmtNum(midY)}`,
      `Q ${fmtNum(xm)} ${fmtNum(midY)} ${fmtNum(xm)} ${fmtNum(midY + r)}`,
      `L ${fmtNum(xm)} ${fmtNum(bottomY - r)}`,
      `Q ${fmtNum(xm)} ${fmtNum(bottomY)} ${fmtNum(xl)} ${fmtNum(bottomY)}`,
    ].join(" ");
    return { w, svg: `<path d="${d}" ${stroke}/>` };
  }
  if (delim === "|") {
    const w = Math.max(5, fontSize * 0.3);
    const xm = x + w / 2;
    return { w, svg: `<line x1="${fmtNum(xm)}" y1="${fmtNum(topY)}" x2="${fmtNum(xm)}" y2="${fmtNum(bottomY)}" ${stroke}/>` };
  }
  if (delim === "‖") {
    const w = Math.max(8, fontSize * 0.48);
    const x1 = x + w * 0.3;
    const x2 = x + w * 0.7;
    return {
      w,
      svg: `<line x1="${fmtNum(x1)}" y1="${fmtNum(topY)}" x2="${fmtNum(x1)}" y2="${fmtNum(bottomY)}" ${stroke}/><line x1="${fmtNum(x2)}" y1="${fmtNum(topY)}" x2="${fmtNum(x2)}" y2="${fmtNum(bottomY)}" ${stroke}/>`,
    };
  }
  if (delim === "⟨") {
    const w = Math.max(7, fontSize * 0.45);
    const d = `M ${fmtNum(x + w * 0.82)} ${fmtNum(topY)} L ${fmtNum(x + w * 0.18)} ${fmtNum(midY)} L ${fmtNum(x + w * 0.82)} ${fmtNum(bottomY)}`;
    return { w, svg: `<path d="${d}" ${stroke}/>` };
  }
  if (delim === "⟩") {
    const w = Math.max(7, fontSize * 0.45);
    const d = `M ${fmtNum(x + w * 0.18)} ${fmtNum(topY)} L ${fmtNum(x + w * 0.82)} ${fmtNum(midY)} L ${fmtNum(x + w * 0.18)} ${fmtNum(bottomY)}`;
    return { w, svg: `<path d="${d}" ${stroke}/>` };
  }
  if (delim === "⌊") {
    const w = Math.max(6, fontSize * 0.4);
    const d = `M ${fmtNum(x + w * 0.25)} ${fmtNum(topY)} L ${fmtNum(x + w * 0.25)} ${fmtNum(bottomY)} L ${fmtNum(x + w * 0.85)} ${fmtNum(bottomY)}`;
    return { w, svg: `<path d="${d}" ${stroke}/>` };
  }
  if (delim === "⌋") {
    const w = Math.max(6, fontSize * 0.4);
    const d = `M ${fmtNum(x + w * 0.75)} ${fmtNum(topY)} L ${fmtNum(x + w * 0.75)} ${fmtNum(bottomY)} L ${fmtNum(x + w * 0.15)} ${fmtNum(bottomY)}`;
    return { w, svg: `<path d="${d}" ${stroke}/>` };
  }
  if (delim === "⌈") {
    const w = Math.max(6, fontSize * 0.4);
    const d = `M ${fmtNum(x + w * 0.85)} ${fmtNum(topY)} L ${fmtNum(x + w * 0.25)} ${fmtNum(topY)} L ${fmtNum(x + w * 0.25)} ${fmtNum(bottomY)}`;
    return { w, svg: `<path d="${d}" ${stroke}/>` };
  }
  if (delim === "⌉") {
    const w = Math.max(6, fontSize * 0.4);
    const d = `M ${fmtNum(x + w * 0.15)} ${fmtNum(topY)} L ${fmtNum(x + w * 0.75)} ${fmtNum(topY)} L ${fmtNum(x + w * 0.75)} ${fmtNum(bottomY)}`;
    return { w, svg: `<path d="${d}" ${stroke}/>` };
  }
  if (delim === "/") {
    const w = Math.max(7, fontSize * 0.45);
    return { w, svg: `<line x1="${fmtNum(x + w * 0.85)}" y1="${fmtNum(topY)}" x2="${fmtNum(x + w * 0.15)}" y2="${fmtNum(bottomY)}" ${stroke}/>` };
  }
  if (delim === "\\") {
    const w = Math.max(7, fontSize * 0.45);
    return { w, svg: `<line x1="${fmtNum(x + w * 0.15)}" y1="${fmtNum(topY)}" x2="${fmtNum(x + w * 0.85)}" y2="${fmtNum(bottomY)}" ${stroke}/>` };
  }
  const w = measureTextWidth(delim, fontSize);
  return {
    w,
    svg: `<text x="${fmtNum(x)}" y="${fmtNum(midY + fontSize * 0.34)}" fill="${escapeXml(color)}" font-family="${MATH_SERIF}" font-size="${fmtNum(fontSize)}">${escapeXml(delim)}</text>`,
  };
}

function measureDelimWidth(delim, h, fontSize) {
  if (!delim) return 0;
  return drawStretchyDelim(delim, 0, 0, h, fontSize, "#000").w;
}

function interAtomGap(prevAtom, nextAtom, fontSize) {
  if (!prevAtom || !nextAtom) return 0;
  if (prevAtom === "open" || nextAtom === "close") return 0;
  if (prevAtom === "punct") return fontSize * 0.18;
  if (nextAtom === "punct") return 0;
  if (prevAtom === "rel" || nextAtom === "rel") return fontSize * 0.26;
  if (prevAtom === "bin" || nextAtom === "bin") return fontSize * 0.2;
  if (prevAtom === "op" && (nextAtom === "ord" || nextAtom === "open" || nextAtom === "op")) {
    return fontSize * 0.16;
  }
  if (prevAtom === "ord" && nextAtom === "op") return fontSize * 0.14;
  return 0;
}

function layoutNode(node, env) {
  const { fontSize, display, color, bold, italicOverride, familyOverride } = env;
  const axis = fontSize * 0.28;
  if (!node) return makeEmptyBox(fontSize);

  switch (node.type) {
    case "space": {
      return { w: Math.max(0, node.em * fontSize), ascent: fontSize * 0.5, descent: 0, axis, atom: "space", svg: () => "" };
    }

    case "style": {
      return { w: 0, ascent: 0, descent: 0, axis, atom: "space", svg: () => "" };
    }

    case "symbol": {
      const text = node.text ?? "";
      if (!text) return makeEmptyBox(fontSize);
      const isBold = Boolean(bold || node.bold);
      const isItalic =
        italicOverride !== undefined
          ? italicOverride
          : node.upright
            ? false
            : Boolean(node.italic);
      const family = familyOverride
        ? familyOverride
        : node.mono
          ? MATH_MONO
          : node.sans
            ? MATH_SANS
            : MATH_SERIF;
      const w = measureTextWidth(text, fontSize, { bold: isBold, italic: isItalic, mono: family === MATH_MONO }) + (isItalic ? fontSize * 0.06 : 0);
      const ascent = fontSize * 0.78;
      const descent = fontSize * 0.24;
      return {
        w,
        ascent,
        descent,
        axis,
        atom: node.atom || "ord",
        svg: (x, y) => {
          const attrs = [
            `x="${fmtNum(x)}"`,
            `y="${fmtNum(y)}"`,
            `fill="${escapeXml(color)}"`,
            `font-family="${family}"`,
            `font-size="${fmtNum(fontSize)}"`,
          ];
          if (isBold) attrs.push(`font-weight="700"`);
          if (isItalic) attrs.push(`font-style="italic"`);
          return `<text ${attrs.join(" ")}>${escapeXml(text)}</text>`;
        },
      };
    }

    case "bigop": {
      const opSize = fontSize * (display ? 1.65 : 1.25);
      const w = measureTextWidth(node.text, opSize) * 0.96 + 2;
      const ascent = opSize * 0.62 + axis * 0.5;
      const descent = opSize * 0.42 - axis * 0.2;
      return {
        w,
        ascent,
        descent: Math.max(fontSize * 0.22, descent),
        axis,
        atom: "op",
        svg: (x, y) => {
          const cy = y - axis + opSize * 0.34;
          return `<text x="${fmtNum(x + w / 2)}" y="${fmtNum(cy)}" text-anchor="middle" fill="${escapeXml(color)}" font-family="${MATH_SERIF}" font-size="${fmtNum(opSize)}">${escapeXml(node.text)}</text>`;
        },
      };
    }

    case "font": {
      let nextEnv = { ...env };
      if (node.font === "mathrm") nextEnv.italicOverride = false;
      else if (node.font === "mathit") nextEnv.italicOverride = true;
      else if (node.font === "mathbf" || node.font === "boldsymbol" || node.font === "bm") {
        nextEnv.bold = true;
        if (node.font === "mathbf") nextEnv.italicOverride = false;
      } else if (node.font === "mathsf") {
        nextEnv.familyOverride = MATH_SANS;
        nextEnv.italicOverride = false;
      } else if (node.font === "mathtt") {
        nextEnv.familyOverride = MATH_MONO;
        nextEnv.italicOverride = false;
      }
      return layoutNode(node.body, nextEnv);
    }

    case "color": {
      const nextColor = /^[#a-zA-Z0-9(),.\s%-]+$/.test(node.color) ? node.color : color;
      return layoutNode(node.body, { ...env, color: nextColor });
    }

    case "phantom": {
      const inner = layoutNode(node.body, env);
      return { ...inner, svg: () => "" };
    }

    case "boxed": {
      const inner = layoutNode(node.body, env);
      const padX = Math.max(4, fontSize * 0.28);
      const padY = Math.max(3, fontSize * 0.2);
      const w = inner.w + padX * 2;
      const ascent = inner.ascent + padY;
      const descent = inner.descent + padY;
      return {
        w,
        ascent,
        descent,
        axis,
        atom: "ord",
        svg: (x, y) => {
          const rectY = y - ascent;
          const rectH = ascent + descent;
          return (
            `<rect x="${fmtNum(x + 0.5)}" y="${fmtNum(rectY + 0.5)}" width="${fmtNum(w - 1)}" height="${fmtNum(rectH - 1)}" rx="2" fill="none" stroke="${escapeXml(color)}" stroke-width="1"/>` +
            inner.svg(x + padX, y)
          );
        },
      };
    }

    case "row": {
      const items = node.items ?? [];
      if (items.length === 0) return makeEmptyBox(fontSize);
      let curEnv = env;
      const boxes = [];
      for (const item of items) {
        if (item.type === "style") {
          if (item.mode === "displaystyle") curEnv = { ...curEnv, display: true };
          else if (item.mode === "textstyle") curEnv = { ...curEnv, display: false };
          else if (item.mode === "scriptstyle") curEnv = { ...curEnv, fontSize: env.fontSize * 0.72, display: false };
          else if (item.mode === "scriptscriptstyle") curEnv = { ...curEnv, fontSize: env.fontSize * 0.55, display: false };
          continue;
        }
        boxes.push(layoutNode(item, curEnv));
      }
      if (boxes.length === 0) return makeEmptyBox(fontSize);

      // Unary + / - at start or after bin/rel/open/punct becomes ord
      for (let i = 0; i < boxes.length; i++) {
        if (boxes[i].atom === "bin") {
          const prev = i > 0 ? boxes[i - 1].atom : null;
          if (!prev || prev === "bin" || prev === "rel" || prev === "open" || prev === "punct" || prev === "op") {
            boxes[i] = { ...boxes[i], atom: "ord" };
          }
        }
      }

      const offsets = [];
      let x = 0;
      let ascent = fontSize * 0.65;
      let descent = fontSize * 0.2;
      let prevAtom = null;
      for (const box of boxes) {
        if (box.atom !== "space") {
          x += interAtomGap(prevAtom, box.atom, fontSize);
          prevAtom = box.atom;
        }
        offsets.push(x);
        x += box.w;
        ascent = Math.max(ascent, box.ascent);
        descent = Math.max(descent, box.descent);
      }
      return {
        w: x,
        ascent,
        descent,
        axis,
        atom: boxes.length === 1 ? boxes[0].atom : "ord",
        svg: (ox, oy) => boxes.map((b, i) => b.svg(ox + offsets[i], oy)).join(""),
      };
    }

    case "script": {
      const base = layoutNode(node.base, env);
      const scriptSize = Math.max(8.5, fontSize * 0.68);
      const scriptEnv = { ...env, fontSize: scriptSize, display: false };
      const subBox = node.sub ? layoutNode(node.sub, scriptEnv) : null;
      const supBox = node.sup ? layoutNode(node.sup, scriptEnv) : null;

      // Display-mode limits above & below operator (e.g., \sum_{i=1}^n)
      if (node.limits && display) {
        const gap = Math.max(2, fontSize * 0.14);
        const w = Math.max(base.w, subBox ? subBox.w : 0, supBox ? supBox.w : 0);
        const ascent = supBox ? base.ascent + gap + supBox.descent + supBox.ascent : base.ascent;
        const descent = subBox ? base.descent + gap + subBox.ascent + subBox.descent : base.descent;
        return {
          w,
          ascent,
          descent,
          axis,
          atom: base.atom,
          svg: (x, y) => {
            let out = base.svg(x + (w - base.w) / 2, y);
            if (supBox) {
              const supBaseY = y - base.ascent - gap - supBox.descent;
              out += supBox.svg(x + (w - supBox.w) / 2, supBaseY);
            }
            if (subBox) {
              const subBaseY = y + base.descent + gap + subBox.ascent;
              out += subBox.svg(x + (w - subBox.w) / 2, subBaseY);
            }
            return out;
          },
        };
      }

      // Standard side superscript / subscript
      const kern = Math.max(0.8, fontSize * 0.05);
      let supShift = 0;
      let subShift = 0;
      if (supBox) {
        supShift = Math.max(base.ascent - fontSize * 0.34, fontSize * 0.38) + supBox.descent * 0.55;
      }
      if (subBox) {
        subShift = Math.max(base.descent + fontSize * 0.12, fontSize * 0.24) + subBox.ascent * 0.35;
      }
      // Prevent sup & sub collision
      if (supBox && subBox) {
        const supBottom = -supShift + supBox.descent;
        const subTop = subShift - subBox.ascent;
        const minClear = fontSize * 0.14;
        if (subTop - supBottom < minClear) {
          const push = (minClear - (subTop - supBottom)) / 2;
          supShift += push;
          subShift += push;
        }
      }
      const scriptW = Math.max(supBox ? supBox.w : 0, subBox ? subBox.w : 0);
      const w = base.w + kern + scriptW + fontSize * 0.04;
      const ascent = Math.max(base.ascent, supBox ? supShift + supBox.ascent : 0);
      const descent = Math.max(base.descent, subBox ? subShift + subBox.descent : 0);
      return {
        w,
        ascent,
        descent,
        axis,
        atom: base.atom,
        svg: (x, y) => {
          let out = base.svg(x, y);
          if (supBox) out += supBox.svg(x + base.w + kern, y - supShift);
          if (subBox) out += subBox.svg(x + base.w + kern, y + subShift);
          return out;
        },
      };
    }

    case "frac": {
      const useDisplay = node.forceDisplay ? true : node.forceText ? false : display;
      const childScale = useDisplay ? 0.88 : 0.75;
      const childEnv = { ...env, fontSize: Math.max(9, fontSize * childScale), display: false };
      const num = layoutNode(node.num, childEnv);
      const den = layoutNode(node.den, childEnv);
      const padX = Math.max(3, fontSize * 0.2);
      const innerW = Math.max(num.w, den.w) + padX * 2;
      const barThick = node.lineThickness ? Math.max(1, fontSize * 0.065) : 0;
      const gap = Math.max(2.5, fontSize * (useDisplay ? 0.2 : 0.14));

      const numAscentAboveAxis = gap + barThick / 2 + num.descent + num.ascent;
      const denDescentBelowAxis = gap + barThick / 2 + den.ascent + den.descent;
      const ascent = axis + numAscentAboveAxis;
      const descent = Math.max(0, denDescentBelowAxis - axis);

      const totalH = ascent + descent;
      const leftW = node.leftDelim ? measureDelimWidth(node.leftDelim, totalH, fontSize) : 0;
      const rightW = node.rightDelim ? measureDelimWidth(node.rightDelim, totalH, fontSize) : 0;
      const w = leftW + innerW + rightW + 2;

      return {
        w,
        ascent,
        descent,
        axis,
        atom: "ord",
        svg: (x, y) => {
          const axisY = y - axis;
          const topY = y - ascent;
          const bottomY = y + descent;
          let out = "";
          if (node.leftDelim) {
            out += drawStretchyDelim(node.leftDelim, x + 1, topY, bottomY, fontSize, color).svg;
          }
          const bx = x + 1 + leftW;
          const numBaseY = axisY - barThick / 2 - gap - num.descent;
          const denBaseY = axisY + barThick / 2 + gap + den.ascent;
          out += num.svg(bx + (innerW - num.w) / 2, numBaseY);
          if (barThick > 0) {
            out += `<line x1="${fmtNum(bx + 1)}" y1="${fmtNum(axisY)}" x2="${fmtNum(bx + innerW - 1)}" y2="${fmtNum(axisY)}" stroke="${escapeXml(color)}" stroke-width="${fmtNum(barThick)}"/>`;
          }
          out += den.svg(bx + (innerW - den.w) / 2, denBaseY);
          if (node.rightDelim) {
            out += drawStretchyDelim(node.rightDelim, bx + innerW, topY, bottomY, fontSize, color).svg;
          }
          return out;
        },
      };
    }

    case "sqrt": {
      const inner = layoutNode(node.radicand, { ...env, display: false });
      const idxBox = node.index ? layoutNode(node.index, { ...env, fontSize: Math.max(8, fontSize * 0.56), display: false }) : null;
      const ruleThick = Math.max(1, fontSize * 0.065);
      const topGap = Math.max(2.5, fontSize * 0.18);
      const hookW = Math.max(9, fontSize * 0.62);
      const extraLeft = idxBox ? Math.max(0, idxBox.w - hookW * 0.45) : 0;
      const rightPad = Math.max(2, fontSize * 0.12);
      const ascent = Math.max(inner.ascent + topGap + ruleThick, idxBox ? inner.ascent * 0.45 + idxBox.ascent + idxBox.descent : 0);
      const descent = Math.max(inner.descent, fontSize * 0.18);
      const w = extraLeft + hookW + inner.w + rightPad;
      return {
        w,
        ascent,
        descent,
        axis,
        atom: "ord",
        svg: (x, y) => {
          const rx = x + extraLeft;
          const topY = y - inner.ascent - topGap;
          const botY = y + descent - 1;
          const midY = y - inner.ascent * 0.15;
          const d = [
            `M ${fmtNum(rx + 1)} ${fmtNum(midY)}`,
            `L ${fmtNum(rx + hookW * 0.34)} ${fmtNum(midY - fontSize * 0.08)}`,
            `L ${fmtNum(rx + hookW * 0.58)} ${fmtNum(botY)}`,
            `L ${fmtNum(rx + hookW)} ${fmtNum(topY)}`,
            `L ${fmtNum(rx + hookW + inner.w + rightPad)} ${fmtNum(topY)}`,
          ].join(" ");
          let out = `<path d="${d}" fill="none" stroke="${escapeXml(color)}" stroke-width="${fmtNum(ruleThick)}" stroke-linejoin="round" stroke-linecap="round"/>`;
          if (idxBox) {
            const ix = rx + hookW * 0.45 - idxBox.w;
            const iy = midY - fontSize * 0.18 - idxBox.descent;
            out += idxBox.svg(ix, iy);
          }
          out += inner.svg(rx + hookW, y);
          return out;
        },
      };
    }

    case "delim": {
      const inner = layoutNode(node.body, env);
      const pad = Math.max(1.5, fontSize * 0.1);
      const ascent = inner.ascent + pad;
      const descent = inner.descent + pad;
      const h = ascent + descent;
      const leftW = measureDelimWidth(node.left, h, fontSize);
      const rightW = measureDelimWidth(node.right, h, fontSize);
      const w = leftW + inner.w + rightW + (leftW ? 2 : 0) + (rightW ? 2 : 0);
      return {
        w,
        ascent,
        descent,
        axis,
        atom: "ord",
        svg: (x, y) => {
          const topY = y - ascent;
          const botY = y + descent;
          let curX = x;
          let out = "";
          if (node.left) {
            const d = drawStretchyDelim(node.left, curX, topY, botY, fontSize, color);
            out += d.svg;
            curX += d.w + 2;
          }
          out += inner.svg(curX, y);
          curX += inner.w + (node.right ? 2 : 0);
          if (node.right) {
            out += drawStretchyDelim(node.right, curX, topY, botY, fontSize, color).svg;
          }
          return out;
        },
      };
    }

    case "big_delim": {
      const half = (fontSize * node.scale) / 2;
      const ascent = axis + half;
      const descent = Math.max(0, half - axis);
      const h = ascent + descent;
      const w = measureDelimWidth(node.delim, h, fontSize);
      return {
        w,
        ascent,
        descent,
        axis,
        atom: "ord",
        svg: (x, y) => drawStretchyDelim(node.delim, x, y - ascent, y + descent, fontSize, color).svg,
      };
    }

    case "accent": {
      const inner = layoutNode(node.base, env);
      const kind = node.kind;
      const sw = Math.max(1, fontSize * 0.068);
      const stroke = `stroke="${escapeXml(color)}" stroke-width="${fmtNum(sw)}" fill="none" stroke-linecap="round" stroke-linejoin="round"`;

      if (kind === "underline") {
        const gap = Math.max(2, fontSize * 0.14);
        const descent = inner.descent + gap + sw;
        return {
          w: inner.w,
          ascent: inner.ascent,
          descent,
          axis,
          atom: "ord",
          svg: (x, y) =>
            inner.svg(x, y) +
            `<line x1="${fmtNum(x)}" y1="${fmtNum(y + inner.descent + gap)}" x2="${fmtNum(x + inner.w)}" y2="${fmtNum(y + inner.descent + gap)}" ${stroke}/>`,
        };
      }
      if (kind === "cancel") {
        return {
          w: inner.w,
          ascent: inner.ascent,
          descent: inner.descent,
          axis,
          atom: "ord",
          svg: (x, y) =>
            inner.svg(x, y) +
            `<line x1="${fmtNum(x)}" y1="${fmtNum(y + inner.descent * 0.7)}" x2="${fmtNum(x + inner.w)}" y2="${fmtNum(y - inner.ascent * 0.9)}" ${stroke}/>`,
        };
      }
      if (kind === "underbrace") {
        const braceH = Math.max(6, fontSize * 0.42);
        const gap = Math.max(2, fontSize * 0.14);
        const descent = inner.descent + gap + braceH;
        return {
          w: inner.w,
          ascent: inner.ascent,
          descent,
          axis,
          atom: "op",
          svg: (x, y) => {
            const topY = y + inner.descent + gap;
            const midY = topY + braceH * 0.5;
            const botY = topY + braceH;
            const xm = x + inner.w / 2;
            const r = Math.min(braceH * 0.5, inner.w * 0.18);
            const d = `M ${fmtNum(x)} ${fmtNum(topY)} Q ${fmtNum(x)} ${fmtNum(midY)} ${fmtNum(x + r)} ${fmtNum(midY)} L ${fmtNum(xm - r)} ${fmtNum(midY)} Q ${fmtNum(xm)} ${fmtNum(midY)} ${fmtNum(xm)} ${fmtNum(botY)} Q ${fmtNum(xm)} ${fmtNum(midY)} ${fmtNum(xm + r)} ${fmtNum(midY)} L ${fmtNum(x + inner.w - r)} ${fmtNum(midY)} Q ${fmtNum(x + inner.w)} ${fmtNum(midY)} ${fmtNum(x + inner.w)} ${fmtNum(topY)}`;
            return inner.svg(x, y) + `<path d="${d}" ${stroke}/>`;
          },
        };
      }
      if (kind === "overbrace") {
        const braceH = Math.max(6, fontSize * 0.42);
        const gap = Math.max(2, fontSize * 0.14);
        const ascent = inner.ascent + gap + braceH;
        return {
          w: inner.w,
          ascent,
          descent: inner.descent,
          axis,
          atom: "op",
          svg: (x, y) => {
            const botY = y - inner.ascent - gap;
            const midY = botY - braceH * 0.5;
            const topY = botY - braceH;
            const xm = x + inner.w / 2;
            const r = Math.min(braceH * 0.5, inner.w * 0.18);
            const d = `M ${fmtNum(x)} ${fmtNum(botY)} Q ${fmtNum(x)} ${fmtNum(midY)} ${fmtNum(x + r)} ${fmtNum(midY)} L ${fmtNum(xm - r)} ${fmtNum(midY)} Q ${fmtNum(xm)} ${fmtNum(midY)} ${fmtNum(xm)} ${fmtNum(topY)} Q ${fmtNum(xm)} ${fmtNum(midY)} ${fmtNum(xm + r)} ${fmtNum(midY)} L ${fmtNum(x + inner.w - r)} ${fmtNum(midY)} Q ${fmtNum(x + inner.w)} ${fmtNum(midY)} ${fmtNum(x + inner.w)} ${fmtNum(botY)}`;
            return inner.svg(x, y) + `<path d="${d}" ${stroke}/>`;
          },
        };
      }

      const gap = Math.max(2, fontSize * 0.12);
      const markH = Math.max(3, fontSize * 0.24);
      const ascent = inner.ascent + gap + markH;
      return {
        w: inner.w,
        ascent,
        descent: inner.descent,
        axis,
        atom: "ord",
        svg: (x, y) => {
          const baseY = y - inner.ascent - gap;
          const xm = x + inner.w / 2;
          let mark = "";
          if (kind === "bar" || kind === "overline") {
            mark = `<line x1="${fmtNum(x)}" y1="${fmtNum(baseY - markH * 0.3)}" x2="${fmtNum(x + inner.w)}" y2="${fmtNum(baseY - markH * 0.3)}" ${stroke}/>`;
          } else if (kind === "hat" || kind === "widehat") {
            const span = kind === "widehat" ? inner.w * 0.48 : Math.min(inner.w * 0.46, fontSize * 0.32);
            mark = `<path d="M ${fmtNum(xm - span)} ${fmtNum(baseY)} L ${fmtNum(xm)} ${fmtNum(baseY - markH)} L ${fmtNum(xm + span)} ${fmtNum(baseY)}" ${stroke}/>`;
          } else if (kind === "check") {
            const span = Math.min(inner.w * 0.46, fontSize * 0.32);
            mark = `<path d="M ${fmtNum(xm - span)} ${fmtNum(baseY - markH)} L ${fmtNum(xm)} ${fmtNum(baseY)} L ${fmtNum(xm + span)} ${fmtNum(baseY - markH)}" ${stroke}/>`;
          } else if (kind === "tilde" || kind === "widetilde") {
            const span = kind === "widetilde" ? inner.w * 0.48 : Math.min(inner.w * 0.46, fontSize * 0.34);
            const my = baseY - markH * 0.45;
            mark = `<path d="M ${fmtNum(xm - span)} ${fmtNum(my + 1)} Q ${fmtNum(xm - span * 0.5)} ${fmtNum(my - markH * 0.7)} ${fmtNum(xm)} ${fmtNum(my)} T ${fmtNum(xm + span)} ${fmtNum(my - 1)}" ${stroke}/>`;
          } else if (kind === "vec" || kind === "overrightarrow") {
            const x1 = x + 0.5;
            const x2 = x + Math.max(inner.w - 0.5, fontSize * 0.45);
            const ay = baseY - markH * 0.45;
            const ah = Math.max(2.2, fontSize * 0.16);
            mark = `<line x1="${fmtNum(x1)}" y1="${fmtNum(ay)}" x2="${fmtNum(x2)}" y2="${fmtNum(ay)}" ${stroke}/><path d="M ${fmtNum(x2 - ah)} ${fmtNum(ay - ah * 0.7)} L ${fmtNum(x2)} ${fmtNum(ay)} L ${fmtNum(x2 - ah)} ${fmtNum(ay + ah * 0.7)}" ${stroke}/>`;
          } else if (kind === "overleftarrow") {
            const x1 = x + 0.5;
            const x2 = x + Math.max(inner.w - 0.5, fontSize * 0.45);
            const ay = baseY - markH * 0.45;
            const ah = Math.max(2.2, fontSize * 0.16);
            mark = `<line x1="${fmtNum(x1)}" y1="${fmtNum(ay)}" x2="${fmtNum(x2)}" y2="${fmtNum(ay)}" ${stroke}/><path d="M ${fmtNum(x1 + ah)} ${fmtNum(ay - ah * 0.7)} L ${fmtNum(x1)} ${fmtNum(ay)} L ${fmtNum(x1 + ah)} ${fmtNum(ay + ah * 0.7)}" ${stroke}/>`;
          } else if (kind === "dot" || kind === "ddot" || kind === "dddot") {
            const r = Math.max(1.1, fontSize * 0.08);
            const cy = baseY - markH * 0.45;
            const count = kind === "dot" ? 1 : kind === "ddot" ? 2 : 3;
            const step = r * 3.1;
            const startX = xm - ((count - 1) * step) / 2;
            for (let k = 0; k < count; k++) {
              mark += `<circle cx="${fmtNum(startX + k * step)}" cy="${fmtNum(cy)}" r="${fmtNum(r)}" fill="${escapeXml(color)}"/>`;
            }
          } else if (kind === "acute") {
            mark = `<line x1="${fmtNum(xm - 2)}" y1="${fmtNum(baseY)}" x2="${fmtNum(xm + 2.5)}" y2="${fmtNum(baseY - markH)}" ${stroke}/>`;
          } else if (kind === "grave") {
            mark = `<line x1="${fmtNum(xm - 2.5)}" y1="${fmtNum(baseY - markH)}" x2="${fmtNum(xm + 2)}" y2="${fmtNum(baseY)}" ${stroke}/>`;
          } else if (kind === "breve") {
            const span = Math.min(inner.w * 0.42, fontSize * 0.28);
            mark = `<path d="M ${fmtNum(xm - span)} ${fmtNum(baseY - markH)} Q ${fmtNum(xm)} ${fmtNum(baseY + 1)} ${fmtNum(xm + span)} ${fmtNum(baseY - markH)}" ${stroke}/>`;
          }
          return inner.svg(x, y) + mark;
        },
      };
    }

    case "stack": {
      const base = layoutNode(node.base, env);
      const smallEnv = { ...env, fontSize: Math.max(8.5, fontSize * 0.65), display: false };
      const over = node.over ? layoutNode(node.over, smallEnv) : null;
      const under = node.under ? layoutNode(node.under, smallEnv) : null;
      const gap = Math.max(2, fontSize * 0.14);
      const w = Math.max(base.w, over ? over.w : 0, under ? under.w : 0);
      const ascent = over ? base.ascent + gap + over.descent + over.ascent : base.ascent;
      const descent = under ? base.descent + gap + under.ascent + under.descent : base.descent;
      return {
        w,
        ascent,
        descent,
        axis,
        atom: base.atom,
        svg: (x, y) => {
          let out = base.svg(x + (w - base.w) / 2, y);
          if (over) out += over.svg(x + (w - over.w) / 2, y - base.ascent - gap - over.descent);
          if (under) out += under.svg(x + (w - under.w) / 2, y + base.descent + gap + under.ascent);
          return out;
        },
      };
    }

    case "matrix": {
      const envName = node.env || "matrix";
      const isSmall = envName === "smallmatrix" || envName === "substack";
      const cellEnv = {
        ...env,
        fontSize: isSmall ? Math.max(8.5, fontSize * 0.72) : fontSize * (display ? 0.96 : 0.86),
        display: envName === "aligned" || envName === "align" || envName === "align*" || envName === "gather" || envName === "gather*" || envName === "dcases" ? display : false,
      };
      const rows = (node.rows ?? []).map((r) => r.map((c) => layoutNode(c, cellEnv)));
      if (rows.length === 0) return makeEmptyBox(fontSize);

      const colCount = Math.max(1, ...rows.map((r) => r.length));
      const colWidths = Array(colCount).fill(0);
      const rowAsc = rows.map((r) => Math.max(cellEnv.fontSize * 0.72, ...r.map((c) => c.ascent)));
      const rowDesc = rows.map((r) => Math.max(cellEnv.fontSize * 0.24, ...r.map((c) => c.descent)));

      for (const r of rows) {
        for (let c = 0; c < r.length; c++) {
          colWidths[c] = Math.max(colWidths[c], r[c].w);
        }
      }

      const isAlignEnv = envName === "aligned" || envName === "align" || envName === "align*" || envName === "split" || envName === "eqnarray" || envName === "eqnarray*";
      const isCasesEnv = envName === "cases" || envName === "dcases" || envName === "rcases";
      const colGap = isSmall ? fontSize * 0.45 : isAlignEnv ? fontSize * 0.12 : isCasesEnv ? fontSize * 1.1 : fontSize * 0.85;
      const rowGap = isSmall ? fontSize * 0.22 : fontSize * 0.42;

      const colAlign = (c) => {
        if (node.colSpec) {
          const letters = node.colSpec.replace(/[^lcr]/g, "");
          if (letters[c]) return letters[c];
        }
        if (isCasesEnv) return "l";
        if (isAlignEnv) return c % 2 === 0 ? "r" : "l";
        return "c";
      };

      const innerW = colWidths.reduce((a, b) => a + b, 0) + Math.max(0, colCount - 1) * colGap;
      const totalH = rows.reduce((sum, _, r) => sum + rowAsc[r] + rowDesc[r], 0) + Math.max(0, rows.length - 1) * rowGap;
      const halfH = totalH / 2;
      const ascent = axis + halfH + fontSize * 0.1;
      const descent = Math.max(0, halfH - axis + fontSize * 0.1);
      const [leftDelim, rightDelim] = MATRIX_DELIMS[envName] ?? ["", ""];
      const leftW = measureDelimWidth(leftDelim, ascent + descent, fontSize);
      const rightW = measureDelimWidth(rightDelim, ascent + descent, fontSize);
      const padL = leftW ? 4 : 0;
      const padR = rightW ? 4 : 0;
      const w = leftW + padL + innerW + padR + rightW;

      return {
        w,
        ascent,
        descent,
        axis,
        atom: "ord",
        svg: (x, y) => {
          const topY = y - ascent;
          const botY = y + descent;
          let out = "";
          if (leftDelim) {
            out += drawStretchyDelim(leftDelim, x, topY, botY, fontSize, color).svg;
          }
          const gridX = x + leftW + padL;
          let curY = y - axis - totalH / 2;
          for (let r = 0; r < rows.length; r++) {
            const baseY = curY + rowAsc[r];
            let curX = gridX;
            for (let c = 0; c < rows[r].length; c++) {
              const cell = rows[r][c];
              const align = colAlign(c);
              const cellX =
                align === "l"
                  ? curX
                  : align === "r"
                    ? curX + (colWidths[c] - cell.w)
                    : curX + (colWidths[c] - cell.w) / 2;
              out += cell.svg(cellX, baseY);
              curX += colWidths[c] + colGap;
            }
            curY += rowAsc[r] + rowDesc[r] + rowGap;
          }
          if (rightDelim) {
            out += drawStretchyDelim(rightDelim, gridX + innerW + padR, topY, botY, fontSize, color).svg;
          }
          return out;
        },
      };
    }

    default:
      return makeEmptyBox(fontSize);
  }
}

// ---------------- Public SVG Renderer ----------------

export function renderMathSvg(latex, options = {}) {
  const source = String(latex ?? "").trim();
  const display = Boolean(options.display);
  const fontSize = Number(options.fontSize) || (display ? 17 : 13.5);
  const color = options.color || "#1f2328";
  const bold = Boolean(options.bold);

  if (!source) {
    return { ok: false, error: "Empty formula", width: 0, height: 0, svg: "", dataUrl: "" };
  }

  try {
    const ast = parseMathAst(source);
    const box = layoutNode(ast, { fontSize, display, color, bold });
    const padX = display ? 12 : 2;
    const padY = display ? 8 : 1.5;

    let width = Math.max(8, Math.ceil(box.w + padX * 2));
    let height;
    let baselineY;

    if (display) {
      height = Math.max(24, Math.ceil(box.ascent + box.descent + padY * 2));
      baselineY = padY + box.ascent;
    } else {
      // Vertically center the math axis inside the SVG viewBox so GPUI's inline image
      // vertical centering aligns the formula baseline seamlessly with surrounding prose.
      const aboveAxis = Math.max(box.ascent - box.axis, fontSize * 0.45);
      const belowAxis = Math.max(box.descent + box.axis, fontSize * 0.45);
      const halfH = Math.max(aboveAxis, belowAxis) + padY;
      height = Math.max(Math.ceil(fontSize * 1.1), Math.ceil(halfH * 2));
      baselineY = height / 2 + box.axis;
    }

    const innerSvg = box.svg(padX, baselineY);
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">${innerSvg}</svg>`;
    return {
      ok: true,
      width,
      height,
      svg,
      dataUrl: svgToDataUrl(svg),
    };
  } catch (e) {
    return { ok: false, error: String(e?.message || e), width: 0, height: 0, svg: "", dataUrl: "" };
  }
}

// ---------------- Inline Math Scanner & Markdown Rewriter ----------------

function isEscaped(str, idx) {
  let bs = 0;
  for (let k = idx - 1; k >= 0 && str[k] === "\\"; k--) bs++;
  return bs % 2 === 1;
}

export function findInlineMathSpans(text) {
  const s = String(text ?? "");
  const spans = [];
  let i = 0;
  while (i < s.length) {
    // Skip inline code spans `...`
    if (s[i] === "`" && !isEscaped(s, i)) {
      let ticks = 0;
      while (i + ticks < s.length && s[i + ticks] === "`") ticks++;
      const close = s.indexOf("`".repeat(ticks), i + ticks);
      if (close >= 0) {
        i = close + ticks;
        continue;
      }
      i += ticks;
      continue;
    }

    // Skip HTML tags (e.g., already-rewritten <img ...>)
    if (s[i] === "<" && /^<\/?[a-zA-Z]/.test(s.slice(i))) {
      const close = s.indexOf(">", i + 1);
      if (close >= 0) {
        i = close + 1;
        continue;
      }
    }

    // Inline \( ... \)
    if (s[i] === "\\" && s[i + 1] === "(" && !isEscaped(s, i)) {
      let j = i + 2;
      while (j < s.length - 1) {
        if (s[j] === "\\" && s[j + 1] === ")" && !isEscaped(s, j)) {
          const expr = s.slice(i + 2, j).trim();
          if (expr) {
            spans.push({ start: i, end: j + 2, expr, display: false });
          }
          i = j + 2;
          j = -1;
          break;
        }
        j++;
      }
      if (j === -1) continue;
    }

    // Inline $$ ... $$
    if (s[i] === "$" && s[i + 1] === "$" && !isEscaped(s, i)) {
      let j = i + 2;
      while (j < s.length - 1) {
        if (s[j] === "$" && s[j + 1] === "$" && !isEscaped(s, j)) {
          const expr = s.slice(i + 2, j).trim();
          if (expr) {
            spans.push({ start: i, end: j + 2, expr, display: true });
          }
          i = j + 2;
          j = -1;
          break;
        }
        j++;
      }
      if (j === -1) continue;
    }

    // Inline $ ... $ (Pandoc/GitHub rules: no space after open, no space before close, no digit after close)
    if (s[i] === "$" && s[i + 1] !== "$" && !isEscaped(s, i)) {
      const nextCh = s[i + 1];
      if (nextCh && !/\s/.test(nextCh)) {
        let j = i + 1;
        let found = -1;
        while (j < s.length) {
          if (s[j] === "\n" && s[j + 1] === "\n") break;
          if (s[j] === "`" && !isEscaped(s, j)) break;
          if (s[j] === "$" && !isEscaped(s, j)) {
            if (s[j + 1] === "$") break;
            const prevCh = s[j - 1];
            const afterCh = s[j + 1];
            // First unescaped $ is the only valid closing candidate; if preceded by whitespace
            // or followed by a digit (currency like "$5 and $10"), reject this opener.
            if (j > i + 1 && prevCh && !/\s/.test(prevCh) && !(afterCh && /[0-9]/.test(afterCh))) {
              found = j;
            }
            break;
          }
          j++;
        }
        if (found > i + 1) {
          const expr = s.slice(i + 1, found);
          // Additional currency guard: skip pure number/comma/period like $5 or $1,000 when next text looks like currency prose
          if (expr.trim()) {
            spans.push({ start: i, end: found + 1, expr, display: false });
            i = found + 1;
            continue;
          }
        }
      }
    }

    i++;
  }
  return spans;
}

export function rewriteInlineMathInMarkdown(markdown, options = {}) {
  const source = String(markdown ?? "");
  if (!source.includes("$") && !source.includes("\\(")) return source;

  const lines = source.split("\n");
  const out = [];
  let fenceChar = "";
  let fenceLen = 0;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const fence = line.match(/^\s{0,3}(`{3,}|~{3,})/);
    if (fence) {
      const marker = fence[1];
      if (!fenceChar) {
        fenceChar = marker[0];
        fenceLen = marker.length;
      } else if (marker[0] === fenceChar && marker.length >= fenceLen) {
        fenceChar = "";
        fenceLen = 0;
      }
      out.push(line);
      continue;
    }
    if (fenceChar) {
      out.push(line);
      continue;
    }

    const spans = findInlineMathSpans(line);
    if (spans.length === 0) {
      out.push(line);
      continue;
    }

    let rebuilt = "";
    let cursor = 0;
    for (const sp of spans) {
      rebuilt += line.slice(cursor, sp.start);
      const rendered = renderMathSvg(sp.expr, {
        display: false,
        fontSize: options.fontSize || 13.5,
        color: options.color || "#1f2328",
        bold: Boolean(options.bold),
      });
      if (rendered.ok && rendered.dataUrl) {
        const alt = escapeXml(sp.expr.slice(0, 80));
        rebuilt += `<img src="${rendered.dataUrl}" width="${rendered.width}" height="${rendered.height}" alt="${alt}" />`;
      } else {
        rebuilt += line.slice(sp.start, sp.end);
      }
      cursor = sp.end;
    }
    rebuilt += line.slice(cursor);
    out.push(rebuilt);
  }

  return out.join("\n");
}
