# The white paper

[`bimetal.pdf`](bimetal.pdf) describes what Bimetal is and why we wrote it: how one contract reaches three hosts, what each boundary promises, how the output stays reproducible, what it costs and where its limits are.

## Rebuilding the PDF

The source is LaTeX in the ACM format, with diagrams in TikZ. [Tectonic](https://tectonic-typesetting.github.io) fetches everything else it needs:

```bash
cd docs/paper && tectonic -X compile bimetal.tex
```

The numbers in its section on costs come from `mise run bench`.
