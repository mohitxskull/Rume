# Rume — Vision

> Status: pre-v1 draft. This is a design reference: it records intent and
> trade-offs. It is not a plan, not a specification of current behavior, and
> not a promise of delivery.

## 1. Executive summary

Rume is an explicit, typed, production-ready application shell for Axum.

It gives teams the coherence of a full-stack framework — structure, generators,
operational tooling — without surrendering their database, configuration, or
runtime to it. No runtime container, no classpath scanning, no mandatory ORM,
no hidden magic.

Rume is not an attempt to out-feature Loco ("Rails in Rust"). It is for teams
that want a framework's coherence but are explicit by design: typed where Loco
is templated, modular where Loco is monolithic, debuggable where magic hides
failure.

## 2. The problem

Rust web development today has a predictable shape:

1. Pick Axum, Actix, or Rocket.
2. Assemble config, env, logging, auth, validation, DB, mail, jobs, testing,
   and deployment by hand.
3. Rebuild the same glue in every project.
4. Discover the gaps (secret handling, env validation, test harnesses) in
   production.

Rume exists to remove that glue tax while keeping Rust's compile-time
guarantees and the team's freedom to choose their own database and tools.

## 3. Target user

A small team (1-8 developers) building an API or SaaS in Rust that:

- Likes Rust's type system and wants to lean on it.
- Wants a framework's structure, generators, and defaults.
- Does not want to hand the framework their database choice.
- Cares about production operations (health, readiness, config inspection).
- Is willing to be explicit in exchange for debuggability.

## 4. Positioning

| What Rume is | What Rume is not |
|---|---|
| Explicit by design — dependencies constructed in code | A runtime DI container |
| Typed — env, config, requests, responses are Rust types | Stringly-typed `config.get<T>("...")` |
| Production-ready — health, readiness, config inspection built in | A "just run it" zero-config experience |
| Modular — optional features in separate crates | A mandatory ORM or database |
| Axum-native — builds on Axum, doesn't reimplement HTTP | Rails-style convention-over-configuration magic |

## 5. Three commitments

1. **Explicit.** Dependencies are constructed in code, not discovered by
   scanning. Configuration is typed, not templated. Transactions are visible,
   not proxied.
2. **Typed.** Env, config, requests, responses, and errors are Rust types
   validated at the boundary. No stringly keys in application code.
3. **Production-ready.** Health, readiness, config inspection, and a doctor
   command are part of the framework, not afterthoughts.

## 6. Three refusals

1. No runtime dependency-injection container.
2. No classpath or module scanning.
3. No mandatory ORM or database.

## 7. Borrowing policy

Rume borrows *shapes*, not machinery:

| Source | Borrow | Do not borrow |
|---|---|---|
| AdonisJS | App skeleton, config/env behavior, generators, lifecycle | Runtime dynamism, `config.get<T>()` |
| Spring Boot | Operational surface, problem details, externalized config | Runtime DI, classpath scanning, AOP |
| Micronaut/Quarkus | Compile-time philosophy, typed config | Build-time complexity, annotation processors |
| Loco | Lessons in what *not* to do | YAML templating, ORM lock-in, fixed config struct |
| Axum/Tower | Everything (the substrate) | — |

## 8. Non-goals

- Feature parity with Loco.
- A runtime service container.
- Classpath or module scanning.
- Hidden transaction boundaries.
- A mandatory ORM.
- JVM-style hot reload.
- A runtime plugin system.

## 9. Pre-v1 rules

- Public framework, but pre-1.0 APIs may break freely.
- Borrow aggressively and rewrite as needed.
- Every feature must justify its maintenance cost.
- No feature is added without a test in the hero workflow.
- The v1 contract is frozen; everything else is experimental.

## 10. Success metrics

- Time from `rume new` to first deployed endpoint: under 30 minutes.
- Lines of configuration for a basic API: under 20.
- Number of concepts to learn before first route: under 5.
- Core crate compile time: competitive with Axum alone.
- Zero stringly-typed config access in generated code.

Rume wins by making the features it *does* have work exceptionally well
together — not by having more features than other Rust frameworks.