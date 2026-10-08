# Sharing the local changes

Publish this Git source checkout, rather than the installed game directory. It contains the existing upstream code, native Skate Controls menu changes, and the Windows keyboard support source under `tools/keyboard/`.

The `.gitignore` excludes local game installations, extracted and converted assets, common MW2/Skate asset formats, downloaded archives, executables, DLLs, installers, build outputs, logs, user bindings, and `.env` with local paths. The upstream `skate/` directory remains included: it is the implementation and converter source, not the extracted Skate 3 game.

Your own MW2, Skate 3, and Minecraft runtime data must remain local. Do not add an asset directory under a different name or force-add ignored files. Git ignore rules do not remove files already committed, so review the staged file list before publishing:

```powershell
git status --short
git diff --cached --name-only
```

Keep `origin` pointing to the original upstream project and use a separate `fork` remote for your own repository. Review the staged source files and run `cargo xtask publish-check` before pushing. Do not publish the installed game folder or force-add ignored files.

See [the keyboard build notes](../tools/keyboard/README.md) to reproduce the mapper and launcher. Game data and dependency binaries are supplied separately on each user's machine.
