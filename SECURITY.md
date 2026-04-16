# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.4.x   | Yes                |
| < 0.4   | No                 |

Only the latest minor release receives security updates. If you are using an older version, please upgrade before reporting.

## Attack Surface

Qartez is a Rust-based static code analysis tool that runs locally as an MCP server. It parses source files using tree-sitter, builds an in-memory index, and serves read-only queries over stdio.

Key properties:

- **No network access.** Qartez does not open sockets, fetch URLs, or communicate over the network.
- **No code execution.** Qartez parses and indexes source code. It never evaluates, compiles, or runs the code it analyzes.
- **No persistent storage of secrets.** The index is an on-disk cache of structural data (symbols, edges, metrics). It contains no credentials or sensitive content.

Given these constraints, the primary attack surface is **malicious input files** - crafted source files that could trigger unexpected behavior in the tree-sitter parsers or in Qartez's own indexing logic (for example, excessive memory allocation, panics, or denial of service).

## Reporting a Vulnerability

If you believe you have found a security vulnerability in Qartez, please report it responsibly. **Do not open a public GitHub issue.**

Email your report to: **issues@qartez.dev**

Please include:

- A description of the vulnerability and its potential impact.
- The Qartez version you tested against.
- Step-by-step instructions to reproduce the issue.
- Any relevant files, logs, or screenshots.
- Whether you would like to be credited in the advisory.

If possible, provide a minimal reproducing input file.

## What to Expect

- **Acknowledgment within 48 hours.** We will confirm receipt of your report and provide a tracking reference.
- **Initial assessment within 7 days.** We will evaluate the report, confirm whether the issue is valid, and share our initial severity assessment.
- **Fix within 90 days.** We aim to release a patch within 90 days of confirming a vulnerability. If the fix requires more time, we will communicate a revised timeline.
- **Coordinated disclosure.** We ask that you do not disclose the vulnerability publicly until a fix is available. We will coordinate with you on the disclosure timeline.

## Scope

This policy covers the Qartez MCP server codebase published at [github.com/kuberstar/qartez-mcp](https://github.com/kuberstar/qartez-mcp).

The following are **out of scope**:

- The Qartez website (qartez.dev).
- Third-party dependencies, unless the vulnerability is triggered through Qartez's usage of them. For issues in upstream crates, please report directly to the upstream maintainer.
- Denial of service through intentionally large repositories (this is a known resource constraint, not a vulnerability).

## Safe Harbor

We support responsible security research. If you act in good faith and follow this policy, we will not pursue legal action against you. We consider security research conducted under this policy to be authorized.

## Credits

We are grateful to security researchers who help keep Qartez safe. With your permission, we will credit you in the release notes and in the security advisory.
