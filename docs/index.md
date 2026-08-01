---
layout: home

hero:
  name: tak
  text: A tachometer for code
  tagline: Pre-v1 CLI performance tracking with retired instruction counts.
  image:
    light: /logo-light.svg
    dark: /logo-dark.svg
    alt: tak logo
  actions:
    - theme: brand
      text: Read the methodology
      link: /guide/methodology
    - theme: alt
      text: CLI reference
      link: /cli/

features:
  - title: Deterministic first
    details: Instruction counts vary by roughly 0.02% between runs, making small regressions visible where wall time cannot.
    link: /guide/methodology
  - title: Timing is context, not a gate
    details: Wall-clock measurements are recorded, but contention makes them too noisy for a tight CI threshold.
    link: /guide/methodology#two-tiers-of-metrics
  - title: History stays in git
    details: Measurements are JSON lines in refs/notes/tak, with no database, account, or hosted service.
    link: /guide/ci
---

::: warning Pre-v1 software
tak is pre-v1. Its CLI, configuration, storage format, and behavior are not finalized and may
change incompatibly between releases. If you need a stable benchmark tool, use
[hyperfine](https://github.com/sharkdp/hyperfine).

These docs are currently AI slop and have not been fully reviewed. They will be reviewed and
finished later.
:::
