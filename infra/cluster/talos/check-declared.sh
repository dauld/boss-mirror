#!/usr/bin/env bash
# infra/cluster/talos/check-declared.sh <node> [live.yaml] — compare a
# node's LIVE Talos machine config against the entries the tree declares
# for it in patches/<node>.yaml (backlog 08430090).
#
#   talosctl -n <ip> get machineconfig -o yaml | infra/cluster/talos/check-declared.sh w-1
#   infra/cluster/talos/check-declared.sh cp-2 cp-2.live.yaml
#
# WHY. The gate depends on machine-config entries (the gate-seed
# directory + kubelet bind on w-1, the image-GC thresholds, the registry
# mirror every node pulls through) that were applied by hand and lived
# on a laptop. A rebuilt w-1 loses the seed mount and the first symptom
# is every gate hanging (d42d4967, one day). Declaring them in the tree
# is half; this is the other half — a comparator, so "the node carries
# what the tree says" is a read and not a belief (CLAUDE.md §Mostly sure
# vs. absolutely sure). The check takes the live config as INPUT because
# no BOSS host holds a talosconfig yet: today David pipes it from his
# Mac, later the forge's `talos-get` verb feeds it (design 1bc4b4ed).
#
# VOCABULARY (design 16115a17), one line per finding, then a summary:
#   MATCH      declared entry present live with the declared value
#   DRIFT      present live with a different value — BOTH printed
#   ABSENT     declared, not live
#   DOUBLED    a declared files/extraMounts entry appears more than once
#              live (w-1 as found 2026-09-15: the seed file entry twice,
#              from two identical hand patches on 2026-09-12)
#   UNDECLARED live, in a class this check reads, named by no declaration
#              — the third state: reported, not a failure
# Classes read for UNDECLARED: machine.files under /var/local, every
# machine.kubelet.extraMounts entry, machine.kubelet.extraConfig imageGC*
# keys, every machine.registries.mirrors host.
#
# EXIT: 0 every declared entry MATCHes (UNDECLARED may be listed);
#       1 any DRIFT, ABSENT or DOUBLED;
#       2 usage — no node, no declaration for it, unreadable input, no
#         machine config in the input, or a live config whose hostname is
#         NOT the node asked for (a wrong target answers instead of
#         erroring; the document names its host, so it is checked);
#      78 python3 is not on this box.
#
# PARSING. The gate image and this pod carry python3 and no PyYAML
# (infra/forge/boss-ci/required-tools.txt lists python3 alone), so the
# YAML reader below is a deliberately SMALL stdlib parser for the subset
# talosctl and these patch files use: block mappings and sequences,
# plain and quoted scalars, `|` block scalars, comments, `---` documents,
# empty `[]`/`{}` and flat flow lists. Every scalar is compared as its
# text (`40` and `"40"` are equal; so are `0o644` and `0o644`). Anchors,
# aliases, tags or nested flow collections stop the check with exit 2
# naming the line rather than reading past it — a parser that guesses
# would report a clean node from a document it did not understand.
#
# The live document is printed by talosctl as MORE THAN ONE resource
# (two copies on every node in David's 2026-09-15 read); the check reads
# the `v1alpha1` one, or the first carrying `machine:`, and says which.
# Counting across copies would read every node as DOUBLED.
set -u

usage() {
    echo "usage: $0 <node> [live-machineconfig.yaml]   (stdin when no file)" >&2
    echo "  declarations: \${BOSS_TALOS_PATCHES:-<beside this script>/patches}/<node>.yaml" >&2
    exit 2
}

node="${1:-}"
[ -n "$node" ] || usage
case "$node" in -h|--help) usage ;; esac
live="${2:--}"

here="${BASH_SOURCE[0]%/*}"
[ "$here" != "${BASH_SOURCE[0]}" ] || here=.
patches="${BOSS_TALOS_PATCHES:-$here/patches}"
declared="$patches/$node.yaml"
if [ ! -f "$declared" ]; then
    echo "check-declared: no declaration for node '$node' at $declared" >&2
    exit 2
fi
if [ "$live" != "-" ] && [ ! -r "$live" ]; then
    echo "check-declared: cannot read live config file: $live" >&2
    exit 2
fi
if ! command -v python3 >/dev/null 2>&1; then
    echo "check-declared: python3 is not on this box — cannot read the config" >&2
    exit 78
fi

# The program rides on fd 3, not stdin: stdin is the live config when no
# file is named, and a heredoc on stdin would BE the program's stdin —
# the check would read an empty document and refuse a healthy node.
python3 /dev/fd/3 "$node" "$declared" "$live" 3<<'PY'
import json
import sys

node, declared_path, live_path = sys.argv[1], sys.argv[2], sys.argv[3]


class Unsupported(Exception):
    pass


# ---- the subset YAML reader ------------------------------------------

def _strip_comment(s):
    """Drop a ` #` comment from a plain scalar line; quotes respected."""
    q = None
    for i, ch in enumerate(s):
        if q:
            if ch == q:
                q = None
        elif ch in ('"', "'"):
            q = ch
        elif ch == "#" and (i == 0 or s[i - 1] in " \t"):
            return s[:i].rstrip()
    return s.rstrip()


def _scalar(text, lineno):
    text = text.strip()
    if text == "":
        return ""
    if text[0] in "&*!":
        raise Unsupported(f"line {lineno}: anchors, aliases and tags are not read")
    if text[0] == '"':
        if not text.endswith('"') or len(text) < 2:
            raise Unsupported(f"line {lineno}: unterminated double-quoted scalar")
        return json.loads(text)
    if text[0] == "'":
        if not text.endswith("'") or len(text) < 2:
            raise Unsupported(f"line {lineno}: unterminated single-quoted scalar")
        return text[1:-1].replace("''", "'")
    if text == "[]":
        return []
    if text == "{}":
        return {}
    if text[0] == "[":
        inner = text[1:-1].strip()
        if not text.endswith("]") or any(c in inner for c in "[]{}"):
            raise Unsupported(f"line {lineno}: nested flow collections are not read")
        return [] if inner == "" else [_scalar(p, lineno) for p in inner.split(",")]
    if text[0] == "{":
        raise Unsupported(f"line {lineno}: flow mappings are not read")
    if text in ("~", "null"):
        return None
    return text


def _split_key(content, lineno):
    """`key: value` / `key:` → (key, rest). Keys may hold ':' (a
    registry host `10.20.0.15:3000:`), so the split is the first ': '
    or a trailing ':'."""
    q = None
    for i, ch in enumerate(content):
        if q:
            if ch == q:
                q = None
            continue
        if ch in ('"', "'") and i == 0:
            q = ch
            continue
        if ch == ":" and (i + 1 == len(content) or content[i + 1] in " \t"):
            key = content[:i].strip()
            if key and key[0] in "\"'":
                key = _scalar(key, lineno)
            return key, content[i + 1:].strip()
    return None, None


class Reader:
    def __init__(self, text):
        self.lines = text.split("\n")
        self.n = len(self.lines)

    def _indent(self, i):
        line = self.lines[i]
        return len(line) - len(line.lstrip(" "))

    def _blank(self, i):
        s = self.lines[i].strip()
        return s == "" or s.startswith("#")

    def _next(self, i):
        while i < self.n and self._blank(i):
            i += 1
        return i

    def _block_scalar(self, i, header, parent_indent):
        """`|`, `|-`, `|+`, `>` (and `>-`, `>+`) at line i; the body is
        the following lines indented deeper than the parent."""
        style, chomp = header[0], header[1:]
        if style not in "|>" or chomp not in ("", "-", "+"):
            raise Unsupported(f"line {i + 1}: block scalar header {header!r} is not read")
        j = i + 1
        body_indent = None
        body = []
        while j < self.n:
            line = self.lines[j]
            if line.strip() == "":
                body.append("")
                j += 1
                continue
            ind = len(line) - len(line.lstrip(" "))
            if ind <= parent_indent or (body_indent is not None and ind < body_indent):
                break
            if body_indent is None:
                body_indent = ind
            body.append(line[body_indent:])
            j += 1
        trailing = 0
        while body and body[-1] == "":
            body.pop()
            trailing += 1
        text = "\n".join(body) if style == "|" else " ".join(body)
        if chomp == "":
            text += "\n"
        elif chomp == "+":
            text += "\n" * (1 + trailing)
        return text, j

    def _value(self, rest, i, indent):
        """The value after `key:` or `- `: inline scalar, block scalar,
        or a nested block on following lines."""
        rest = _strip_comment(rest)
        if rest.startswith("|") or rest.startswith(">"):
            return self._block_scalar(i, rest, indent)
        if rest != "":
            return _scalar(rest, i + 1), i + 1
        j = self._next(i + 1)
        if j < self.n and self._indent(j) > indent:
            return self._node(j, self._indent(j))
        if j < self.n and self._indent(j) == indent and self.lines[j].strip().startswith("- "):
            # A sequence at its key's own column (`files:` / `- path:`),
            # which YAML allows and some writers emit.
            return self._sequence(j, indent)
        return None, i + 1

    def _node(self, i, indent):
        content = self.lines[i].strip()
        if content == "-" or content.startswith("- "):
            return self._sequence(i, indent)
        return self._mapping(i, indent)

    def _mapping(self, i, indent):
        out = {}
        while i < self.n:
            i = self._next(i)
            if i >= self.n or self._indent(i) < indent:
                break
            if self._indent(i) > indent:
                raise Unsupported(f"line {i + 1}: unexpected indentation")
            content = self.lines[i].strip()
            if content.startswith("- "):
                break
            key, rest = _split_key(content, i + 1)
            if key is None:
                raise Unsupported(f"line {i + 1}: expected `key: value`, got {content!r}")
            value, i = self._value(rest, i, indent)
            if key in out:
                raise Unsupported(f"line {i}: duplicate key {key!r}")
            out[key] = value
        return out, i

    def _sequence(self, i, indent):
        out = []
        while i < self.n:
            i = self._next(i)
            if i >= self.n or self._indent(i) < indent:
                break
            if self._indent(i) > indent:
                raise Unsupported(f"line {i + 1}: unexpected indentation")
            content = self.lines[i].strip()
            if content == "-":
                value, i = self._value("", i, indent)
                out.append(value)
                continue
            if not content.startswith("- "):
                break
            item = content[2:].lstrip(" ")
            item_indent = indent + (len(content) - len(item))
            if item == "-" or item.startswith("- "):
                # `- - nested`: a sequence that begins on the dash line.
                self.lines[i] = " " * item_indent + item
                value, i = self._sequence(i, item_indent)
                out.append(value)
                continue
            key, _rest = _split_key(item, i + 1)
            if key is not None and item[0] not in "[{\"'":
                # `- key: value`: a mapping that begins on the dash line;
                # the rest of its keys sit at the item's column.
                self.lines[i] = " " * item_indent + item
                value, i = self._mapping(i, item_indent)
                out.append(value)
                continue
            value, i = self._value(item, i, indent)
            out.append(value)
        return out, i

    def documents(self):
        docs = []
        start = 0
        for k in range(self.n + 1):
            end = k == self.n
            marker = (not end) and (self.lines[k] == "---" or self.lines[k].startswith("--- "))
            if end or marker or (not end and self.lines[k] == "..."):
                sub = Reader("\n".join(self.lines[start:k]))
                j = sub._next(0)
                if j < sub.n:
                    docs.append(sub._node(j, sub._indent(j))[0])
                start = k + 1
        return docs


def read_yaml(text):
    return Reader(text).documents()


# ---- pick the machine config out of the live document ------------------

def find_configs(docs):
    """(id, config) for every document carrying a machine config: a
    talosctl resource (`spec:` holding the config, as a mapping or as a
    block string) or a raw config with `machine:` at the top."""
    found = []
    for d in docs:
        if not isinstance(d, dict):
            continue
        rid = ((d.get("metadata") or {}) if isinstance(d.get("metadata"), dict) else {}).get("id")
        spec = d.get("spec")
        if isinstance(spec, str):
            for inner in read_yaml(spec):
                if isinstance(inner, dict) and isinstance(inner.get("machine"), dict):
                    found.append((rid, inner))
        elif isinstance(spec, dict) and isinstance(spec.get("machine"), dict):
            found.append((rid, spec))
        elif isinstance(d.get("machine"), dict):
            found.append((rid, d))
    return found


def get(d, *path):
    for p in path:
        if not isinstance(d, dict):
            return None
        d = d.get(p)
    return d


# ---- the comparison ------------------------------------------------------

def show(v):
    """A scalar as itself, a compound value as compact JSON."""
    v = norm(v)
    return v if isinstance(v, str) else json.dumps(v, sort_keys=True, separators=(",", ":"))


def norm(v):
    """Scalars compare as text; None as empty."""
    if v is None:
        return ""
    if isinstance(v, (str, int, float, bool)):
        return str(v)
    if isinstance(v, list):
        return [norm(x) for x in v]
    if isinstance(v, dict):
        return {str(k): norm(x) for k, x in v.items()}
    return str(v)


findings = []  # (verdict, text)


def say(verdict, text):
    findings.append((verdict, text))
    print(f"{verdict:<10} {text}")


def keyed_list(cfg, path, key):
    items = get(cfg, *path)
    if items is None:
        return []
    if not isinstance(items, list):
        raise Unsupported(f"{'.'.join(path)} is not a list")
    out = []
    for it in items:
        if not isinstance(it, dict) or key not in it:
            raise Unsupported(f"an entry of {'.'.join(path)} has no {key!r}")
        out.append((str(it[key]), it))
    return out


def compare_list(name, dec, live, key, undeclared_when):
    dec_items = keyed_list(dec, name.split("."), key)
    live_items = keyed_list(live, name.split("."), key)
    seen = {}
    for k, _ in dec_items:
        if k in seen:
            print(f"check-declared: the declaration itself names {name}[{k}] twice — fix the declaration", file=sys.stderr)
            sys.exit(2)
        seen[k] = True
    for k, d in dec_items:
        copies = [it for kk, it in live_items if kk == k]
        label = f"{name}[{k}]"
        if not copies:
            say("ABSENT", f"{label}  declared, not live: {show(d)}")
            continue
        if len(copies) > 1:
            say("DOUBLED", f"{label}  live has {len(copies)} copies of a declared entry (declared once)")
        differing = [c for c in copies if norm(c) != norm(d)]
        if differing:
            say("DRIFT", f"{label}  declared {show(d)}  live {show(differing[0])}")
        elif len(copies) == 1:
            say("MATCH", label)
    for k, it in live_items:
        if k not in seen and undeclared_when(k):
            say("UNDECLARED", f"{name}[{k}]  live, no declaration names it: {show(it)}")


def compare_map(name, dec, live, undeclared_when):
    path = name.split(".")
    dec_map = get(dec, *path) or {}
    live_map = get(live, *path) or {}
    if not isinstance(dec_map, dict) or not isinstance(live_map, dict):
        raise Unsupported(f"{name} is not a mapping")
    for k, d in dec_map.items():
        label = f"{name}.{k}" if not isinstance(d, dict) else f"{name}[{k}]"
        if k not in live_map:
            say("ABSENT", f"{label}  declared, not live: {show(d)}")
        elif norm(live_map[k]) != norm(d):
            say("DRIFT", f"{label}  declared {show(d)}  live {show(live_map[k])}")
        else:
            say("MATCH", label)
    for k, v in live_map.items():
        if k not in dec_map and undeclared_when(k):
            label = f"{name}.{k}" if not isinstance(v, dict) else f"{name}[{k}]"
            say("UNDECLARED", f"{label}  live, no declaration names it: {show(v)}")


try:
    with open(declared_path) as f:
        dec_docs = read_yaml(f.read())
except Unsupported as e:
    print(f"check-declared: cannot read {declared_path}: {e}", file=sys.stderr)
    sys.exit(2)
if len(dec_docs) != 1 or not isinstance(dec_docs[0], dict) or not isinstance(dec_docs[0].get("machine"), dict):
    print(f"check-declared: {declared_path} is not one machine-config fragment (`machine:` at the top)", file=sys.stderr)
    sys.exit(2)
dec = dec_docs[0]

try:
    text = sys.stdin.read() if live_path == "-" else open(live_path).read()
    configs = find_configs(read_yaml(text))
except Unsupported as e:
    print(f"check-declared: cannot read the live config: {e}", file=sys.stderr)
    sys.exit(2)
except OSError as e:
    print(f"check-declared: cannot read the live config: {e}", file=sys.stderr)
    sys.exit(2)
if not configs:
    print("check-declared: no machine config in the input (expected `talosctl get machineconfig -o yaml`, or a config with `machine:` at the top)", file=sys.stderr)
    sys.exit(2)
chosen = next((c for c in configs if c[0] == "v1alpha1"), configs[0])
rid, live = chosen
hostname = get(live, "machine", "network", "hostname")
if hostname is not None and str(hostname) != node:
    print(f"check-declared: the live config says hostname {hostname!r}; asked to check {node!r} — refusing to compare another node's config", file=sys.stderr)
    sys.exit(2)
print(f"read       machineconfig id={rid or '(raw)'} ({len(configs)} config document(s) in input; hostname {hostname or '(unset)'}) against {declared_path}")

try:
    compare_list("machine.files", dec, live, "path", lambda p: p.startswith("/var/local/"))
    compare_list("machine.kubelet.extraMounts", dec, live, "destination", lambda _d: True)
    compare_map("machine.kubelet.extraConfig", dec, live, lambda k: str(k).startswith("imageGC"))
    compare_map("machine.registries.mirrors", dec, live, lambda _h: True)
except Unsupported as e:
    print(f"check-declared: cannot compare: {e}", file=sys.stderr)
    sys.exit(2)

counts = {v: 0 for v in ("MATCH", "DRIFT", "ABSENT", "DOUBLED", "UNDECLARED")}
for v, _ in findings:
    counts[v] += 1
hard = counts["DRIFT"] + counts["ABSENT"] + counts["DOUBLED"]
verdict = "every declared entry matches" if hard == 0 else f"{hard} finding(s) against the declaration"
print(
    f"check-declared: {node}: {counts['MATCH']} match, {counts['DRIFT']} drift, "
    f"{counts['ABSENT']} absent, {counts['DOUBLED']} doubled, {counts['UNDECLARED']} undeclared — {verdict}"
)
sys.exit(1 if hard else 0)
PY
