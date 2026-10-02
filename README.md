# AVNI Phone download page

https://sehgalautoriders.github.io/avni/ - Android only, for Ravi and family (unlisted, noindex).

- `index.html` - the page (EN / हिं / मरा), reads the version from the latest release.
- `publish.ps1` - puts the current promoted AVNI Phone APK here as release `v<version>` with asset `AVNI-Phone.apk`.
  Runs from the scheduled task `\AVNI\AVNI Phone Download Page` in the day window (06:30-22:30). It never builds,
  versions or installs (Directive 0015); it checks the sha256 against the canonical provenance first.
