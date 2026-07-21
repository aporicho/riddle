# Architecture and review-size standards

MagicPaper is an application hosted by Remagic. It owns its document model and
user experience, but it does not own raw input, the physical panel, process
supervision, or system/application switching.

Modules are divided by responsibility: application orchestration, lifecycle,
input interpretation, rendering, Oracle transport, and persistent domain data.
Platform adapters may depend on the domain; domain modules must not depend on a
specific device backend.

- Production files target at most 400 physical lines and fail above 500 by
  default. A responsibility that is demonstrably clearer in one file may use
  an exact-path, reasoned upper bound in `architecture-exceptions.tsv`; globs
  and directory-wide exemptions are not allowed.
- Test and fixture files may contain up to 800 lines.
- Generated, vendored, and upstream patch files stay isolated and are excluded.
- Functions target at most 60 lines and production Rust functions fail above
  100 lines. Long workflows are decomposed into named, testable phases.

Run `scripts/check-architecture.sh` before committing. Passing the numerical
gate does not permit a module to combine unrelated responsibilities, nor does
an exception excuse mixed ownership.
