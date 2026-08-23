# clojurust and cljrsh tutorials

These tutorials teach the two binaries this workspace produces: `cljrs`, the
language runtime with its JIT and AOT compiler, and `cljrsh`, the
babashka-style scripting shell built on top of it. Every code example was run
against the current tree before it was written down; where output is shown,
that output is real.

Work through them in order the first time. Each tutorial builds on names and
files introduced by the previous one, and later tutorials assume the setup
from [Getting started](01-getting-started.md).

| Tutorial | What you learn |
|----------|----------------|
| [01 Getting started](01-getting-started.md) | Install both binaries, run first programs, choose between them |
| [02 The REPL](02-repl.md) | Tab completion, `doc`/`source`/`apropos`, interrupts, nREPL and editors |
| [03 Scripting](03-scripting.md) | Shebang scripts, stdin streaming, bb.edn tasks, dependencies, pods, uberscript, nushell |
| [04 Kubernetes](04-kubernetes.md) | The `k8s` client, manifests as data, overlays, kustomize interop |
| [05 AWS](05-aws.md) | The `aws` client: S3, presigned URLs, JSON-protocol services |
| [06 Terraform](06-terraform.md) | Stacks as EDN, fragment functions, plans as data, policy gates |
| [07 Schemas with malli](07-schemas.md) | Validation, humanized errors, string coercion, defaults |
| [08 Layered configuration](08-configuration.md) | CUE-style unification, `override`, schema-checked loading |
| [09 Templating with data](09-templates.md) | Functions instead of template languages; YAML/JSON emission |

## Prerequisites

You need a Rust toolchain (for installation) and, for the later tutorials,
the tools they exercise: tutorial 04 needs a Kubernetes cluster and
`kubectl`, tutorial 05 needs S3-compatible credentials, and tutorial 06 needs
`tofu` or `terraform` on PATH. Tutorials 01 through 03 and 07 through 09 need
nothing beyond the binaries themselves and network access for dependency
downloads.
