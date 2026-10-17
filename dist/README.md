# Release tree

Everything in this folder mirrors the game folder. `kotor-launcher make-release
--from dist --out <updates dir>` hashes it into a signed update.

Before publishing, copy the built DLL in:

    cp target/i686-pc-windows-gnu/release/kotor_coop.dll dist/kotor-coop/

Game patches go in the same tree, e.g. `dist/Override/my_patch.2da`. Original
game files the update overwrites are backed up and restored on uninstall.
This README is excluded from releases by `scripts/release.sh`.
