---
layout: home
title: Track command-line performance across releases
description: Define CLI benchmarks in tak.toml and store results in Git. Measure elapsed time and, where Valgrind is available, instruction counts. Experimental pre-v1 software.

hero:
  name: tak
  text: Track command-line performance across releases
  tagline: Define benchmarks in tak.toml and keep their results in Git. tak records elapsed time and, where Valgrind is available, instruction counts. It is experimental and may change incompatibly between releases.
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
  - title: Instruction counts
    details: With Valgrind, tak counts the instructions a command executes. The methodology documents the measured variation, workloads, and limits of using these counts to detect regressions.
    link: /guide/methodology
  - title: Elapsed time
    details: Wall-clock measurements are recorded, but contention makes them too noisy for a tight CI threshold.
    link: /guide/methodology#two-tiers-of-metrics
  - title: Benchmark history
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
