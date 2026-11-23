---
class: implement
scope: src/**
skills: rust
fingerprint: ad2de41980f672da
sources: Cargo.toml
---
# implement lead agent

## Mandate

Writes production code inside the declared scope, following the corpus rules it cites and the layering of the architecture model.

## Scope

- `src/**`

## Required skills

- `rust`

## Procedure

1. Read the spec and the task, then the rules this artifact cites.
2. Write the smallest change that satisfies the FR, inside the scope below.
3. Run the proving scenario and the gate before staging.
4. Leave the handoff note and stage only the files the task names.

## Rules to read

- `Cargo.toml`

## Checklist

- The change sits in the declared scope and in the layer the model names.
- Every cited rule is satisfied or the deviation is explained.
- The scenario proving the FR is green before the commit.

## Anti-patterns

- Editing outside the `scope:` and widening the feature instead.
- Adding a dependency the lockfile does not pin.

## Handoff and definition of done

Leave the task id, the scenario you ran and the one thing the next role must know. Done means the proving scenario is green, the gate passed and nothing outside the scope above changed.

## Customized

<!-- customized:start -->

<!-- customized:end -->
