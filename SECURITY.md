# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

Rume is a pre-1.0 application framework for Axum. If you discover a security
vulnerability, please report it to the maintainer by opening a private
[GitHub Security Advisory](https://github.com/mohitxskull/Rume/security/advisories/new)
or by emailing [mohitxskull@gmail.com](mailto:mohitxskull@gmail.com).

Please include:

- A description of the vulnerability and its impact
- Steps to reproduce the issue
- The affected version(s) of Rume
- Any relevant stack traces or error output
- Suggested remediation, if you have one

## Disclosure Policy

We will acknowledge receipt of your report within 5 business days and provide an
initial assessment within 10 business days. We ask that you give us reasonable
time to investigate and address the issue before public disclosure.

For pre-1.0 software, security fixes may be released as part of regular version
bumps rather than as dedicated security patches, since there are no stability
guarantees yet.

## Scope

In scope:

- The `rume`, `rume-cli`, and `rume-macros` crates
- Generated project templates
- Workbench reference applications

Out of scope:

- Third-party dependencies (report those to their respective maintainers)
- Deployment environments and infrastructure you operate yourself

## Acknowledgments

Security researchers who responsibly disclose vulnerabilities will be
acknowledged in the project's release notes, if they wish.
