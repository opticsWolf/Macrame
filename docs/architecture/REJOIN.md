<!--nav-->
[index](README.md)
<!--/nav-->

# Reassembling the single file

The split is lossless. Concatenating the seven section files in order, with each file's
`<!--nav-->…<!--/nav-->` header stripped and the index table at the end of `README.md`
dropped, reproduces the former `docs/Macrame v0.5.4.md`.

Since the split, cross-references have been turned into links. That is the only change, and it
is markup rather than text: deleting every `<a id="…"></a>` anchor and reducing every
`[text](target)` to its `text` turns the rejoined file back into the original, byte for byte.
Both halves of that claim are checked — the rejoin, and the reduction.

Run from this directory:

```bash
for f in README.md s0-s3-foundations.md s4-schema.md s5-modules.md s6-s10-flows-to-dependencies.md s11-s12-milestones-and-risks.md s13-decision-register.md appendices.md; do awk 'BEGIN{skip=1} /^<!--index-->$/{exit} /^<!--\/nav-->$/{skip=0; getline; next} !skip{print}' "$f"; done > "../Macrame v0.5.4.md"
```

The loop matters, and the obvious one-liner is wrong: passing every file to a single `awk`
invocation lets the `exit` that drops `README.md`'s index table end the whole run at the first
file. `awk` has to be restarted per file so its `skip` state resets.

This is a reconstruction path, not a backup. It does not guard against ordinary edits to the
prose — the section files *are* the document now. What it catches is a structural mistake: a
section dropped, a file renamed out of the list, or the ordering disturbed.
