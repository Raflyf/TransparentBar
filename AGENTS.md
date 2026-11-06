# Release policy

- Increase the release version only when adding new user-facing features.
- Bug fixes, refactors, logo/design changes, documentation, and build maintenance keep the current release version. Do not create a new version tag for these changes.
- Identify maintenance builds by Git commit SHA and CI run, not by increasing the release version.
- Keep version metadata consistent across Cargo, Windows resources, installer, and the About dialog.
- Do not rewrite published tags or delete existing release assets to apply this policy.
