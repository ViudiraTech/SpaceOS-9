# base 1.0.0 third-party notices

`base` 包含以下组件。每个组件按其自己的许可证提供；`base` 整体不改变这些许可证。

## SpaceOS-9 components

- `spk`, `spk-sandbox`, `systemg`
- Copyright (C) 2026 ViudiraTech.
- Source-available; this repository does not grant an open-source license.
- License identifier: `LicenseRef-Proprietary`.

## BusyBox 1.36.1

- `/bin/busybox`, `/bin/sh` (symlink to `busybox`)
- Copyright (C) 1998-2023 Erik Andersen, Rob Landley, Denys Vlasenko and others.
- License: GNU General Public License version 2 only (`GPL-2.0-only`).
- Source: https://busybox.net/downloads/busybox-1.36.1.tar.bz2
- The build script `tools/build_busybox.sh` pins the source archive and its
  SHA-256. BusyBox is distributed in the hope that it will be useful, but
  WITHOUT ANY WARRANTY; without even the implied warranty of
  MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General
  Public License for more details.

## musl 1.2.5

- `/lib/ld-musl-x86_64.so.1`, `/lib/libc.musl-x86_64.so.1` (symlink)
- Copyright (C) 2005-2020 Rich Felker, et al.
- License: MIT.
- Source: https://musl.libc.org/releases/musl-1.2.5.tar.gz
- Permission is hereby granted, free of charge, to any person obtaining a copy
  of this software and associated documentation files (the "Software"), to deal
  in the Software without restriction, including without limitation the rights
  to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
  copies of the Software, and to permit persons to whom the Software is
  furnished to do so, subject to the following conditions: the above copyright
  notice and this permission notice shall be included in all copies or
  substantial portions of the Software. THE SOFTWARE IS PROVIDED "AS IS",
  WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED.

## CA certificates

- `/etc/ssl/certs/ca-certificates.crt`
- Mozilla CA Certificate Store.
- License: Mozilla Public License 2.0 (`MPL-2.0`).
- Source: https://curl.se/ca/cacert.pem and the Mozilla CA certificate
  program (https://www.mozilla.org/en-US/about/governance/policies/security-group/certs/).

## Full license texts

GPL-2.0, MIT and MPL-2.0 texts are available from their respective projects
and from the source URLs above. The SpaceOS-9 repository's `README` describes
the terms for the SpaceOS components.
