# Release policy

- Increase the release version when adding new user-facing features or shipping bug fixes. Use a patch increment for bug-fix releases.
- Refactors without behavior changes, logo/design changes, documentation, and build maintenance alone keep the current release version. Do not create a new version tag for these changes.
- Identify non-release maintenance builds by Git commit SHA and CI run.
- Keep version metadata consistent across Cargo, Windows resources, installer, and the About dialog.
- Do not rewrite published tags or delete existing release assets to apply this policy.
