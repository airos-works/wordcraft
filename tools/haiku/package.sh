#!/usr/bin/env bash
# Builds WordCraft for air/OS and packages it as an .hpkg.
#
# The binary is cross-compiled here (tools/haiku/build.sh); the resources,
# MIME attributes and the package itself are made on a Haiku machine, because
# rc, xres, mimeset and package are Haiku tools: the test VM by default
# (tools/haiku/vm/haiku.sh), or the workstation with WC_PKG_HOST=ws.
# The package lands in artifacts/haiku/. Usage: tools/haiku/package.sh
set -euo pipefail
ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT"
RES=apps/wordcraft-haiku/resources
NAME=$(awk '$1 == "name" { print $2; exit }' $RES/WordCraft.PackageInfo)
VERSION=$(awk '$1 == "version" { print $2; exit }' $RES/WordCraft.PackageInfo)
FILE=$NAME-$VERSION-x86_64.hpkg
case ${WC_PKG_HOST:-vm} in
	ws) RUN="bash tools/haiku/ws.sh" ;;
	*) RUN="bash tools/haiku/vm/haiku.sh" ;;
esac

bash tools/haiku/build.sh
mkdir -p artifacts/haiku
STAGE=$(mktemp -d "${TMPDIR:-/tmp}/wordcraft-pkg-XXXXXX")
trap 'rm -rf -- "$STAGE"' EXIT
. tools/haiku/env.sh
"$HAIKU_CROSS-strip" -o "$STAGE/WordCraft" target/x86_64-unknown-haiku/release/WordCraft
cp $RES/WordCraft.rdef $RES/WordCraft.hvif $RES/WordCraft.PackageInfo $RES/wordcraft-post-install.sh README.md \
	LICENSE-MIT LICENSE-APACHE NOTICE docs/haiku.md "$STAGE/"
DOCS=$STAGE
# egui's default fonts come from the storytold/egui commit patched in Cargo.toml (a git checkout).
mkdir -p "$DOCS/licenses/epaint_default_fonts"
EGUI_REV=$(sed -n 's/^epaint_default_fonts = .*rev = "\([0-9a-f]*\)".*/\1/p' Cargo.toml)
cp "$CARGO_HOME"/git/checkouts/egui-*/"${EGUI_REV:0:7}"/crates/epaint_default_fonts/fonts/*.txt "$DOCS/licenses/epaint_default_fonts/"
# hayro, the PDF renderer the print backend uses, embeds fonts, CMaps and colour profiles.
for crate in hayro-cmap hayro-interpret hayro-jpeg2000; do
	version=$(awk -F '"' -v name="$crate" '$0 == "name = \"" name "\"" { found = 1; next }
		found && /^version = / { print $2; exit }' Cargo.lock)
	mkdir -p "$DOCS/licenses/$crate"
	cp "$CARGO_HOME"/registry/src/*/"$crate-$version"/assets/*LICENSE* "$DOCS/licenses/$crate/"
done

REMOTE=/tmp/wordcraft-package
tar -C "$STAGE" -czf - . | $RUN "rm -rf $REMOTE && mkdir -p $REMOTE/in && tar -xzf - -C $REMOTE/in && cd $REMOTE && \
	S=$REMOTE/stage && DOCS=\$S/documentation/packages/wordcraft && \
	mkdir -p \$S/apps \$S/data/deskbar/menu/Applications \$S/boot/post-install \$DOCS && \
	cp in/wordcraft-post-install.sh \$S/boot/post-install/wordcraft.sh && chmod 755 \$S/boot/post-install/wordcraft.sh && \
	cp in/WordCraft \$S/apps/WordCraft && \
	(cd in && rc -o WordCraft.rsrc WordCraft.rdef) && xres -o \$S/apps/WordCraft in/WordCraft.rsrc && \
	cp in/README.md in/LICENSE-MIT in/LICENSE-APACHE in/NOTICE in/haiku.md \$DOCS/ && cp -R in/licenses \$DOCS/ && \
	ln -s ../../../../apps/WordCraft \$S/data/deskbar/menu/Applications/WordCraft && \
	cp in/WordCraft.PackageInfo \$S/.PackageInfo && \
	(cd \$S && mimeset --all -f --mimedb data/mime_db --mimedb /boot/system/data/mime_db apps/WordCraft) && \
	rmattr META:PPATH \$S/data/mime_db/application/x-vnd.storytold-wordcraft && \
	package create -C \$S $REMOTE/$FILE && rm -rf \$S && package list $REMOTE/$FILE | tail -3" >&2
$RUN "cat $REMOTE/$FILE" > "artifacts/haiku/$FILE"
ls -l "artifacts/haiku/$FILE" >&2
echo "artifacts/haiku/$FILE"
