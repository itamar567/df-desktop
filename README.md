# Building
Set up the following structure:
- project_root/[desktop](https://github.com/itamar567/df-desktop)
- project_root/[df-cache-layer](https://github.com/itamar567/df-ruffle-cache-layer)
- project_root/[ruffle](https://github.com/ruffle-rs/ruffle)

Note that you will need to revert commit b77f1fe7e849493de484f382d9cf9b4b451678e7, as it depends on custom ruffle patches that don't exist in upstream.
then, run inside `project_root/desktop`:
```cargo build --release```
and the output will be in `project_root/desktop/target/release/itmr-dragonfable-launcher`
