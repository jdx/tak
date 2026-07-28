---
layout: home

hero:
  name: tak
  text: A tachometer for code
  tagline: A half-baked experiment in measuring CLI work with retired instruction counts.
  image:
    src: /favicon.svg
    alt: tak logo
  actions:
    - theme: brand
      text: Read the experiment
      link: /guide/experiment
    - theme: alt
      text: CLI reference
      link: /cli/

features:
  - title: Deterministic first
    details: Instruction counts vary by roughly 0.02% between runs, making small regressions visible where wall time cannot.
    link: /guide/experiment
  - title: Timing is context, not a gate
    details: Wall-clock measurements are recorded, but contention makes them too noisy for a tight CI threshold.
    link: /guide/experiment#two-tiers-of-metrics
  - title: History stays in git
    details: Measurements are JSON lines in refs/notes/tak, with no database, account, or hosted service.
    link: /guide/ci
---

::: danger GO AWAY
tak is a half-baked experiment. Do not use it, depend on it, package it, or expect support,
stability, or a roadmap. If you actually need to benchmark a command, use
[hyperfine](https://github.com/sharkdp/hyperfine).
:::
