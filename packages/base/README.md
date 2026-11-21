# base

Meta package for the SpaceOS base runtime.

- P5 images install `base` plus the full `base-core` package.
- Desktop images provide `base-core` through `spk-runtime`; this avoids
  replacing Alpine's musl/BusyBox/systemg with the P5 minimal userland.
