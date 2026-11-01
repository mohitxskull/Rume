# Rule — Tech stack

<!-- Fill in per project. Until then this rule binds through its two standing
     laws below; everything else is a placeholder to be replaced, not guidance
     to be guessed at. -->

## Standing laws (any stack)

- **Every dependency version is a concrete pin.** A range is not a pin — APIs
  and signatures move between minor and prerelease versions.
- The authoritative pin table lives here or at the path named below; when it
  and reality disagree, fix the pin table in the same change.
- **Harness floor: opencode v2.2.x.** The `.opencode/` wiring
  (`opencode.json` `instructions` globs and `permissions` rules, `skills/` and
  `commands/` auto-discovery) is verified against opencode v2.2.5. A new major
  version needs re-verification before adopting; a minor bump within v2 does
  not.

## Stack

- TODO: language, framework, storage — with the *decisions made against*
  alternatives ("X chosen over Y because …"), so an agent does not re-litigate.
- TODO: where the version table lives.
