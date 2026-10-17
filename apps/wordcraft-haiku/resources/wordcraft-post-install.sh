#!/bin/sh
# WordCraft's post-install script (boot/post-install/wordcraft.sh in the package).
#
# Registers the application with the MIME database, whose packaged record names the machine
# the package was built on, and makes WordCraft the preferred application of the Word
# document types that have none, so a double-click in Tracker opens them before WordCraft has
# ever run (it does the same at each start; the types are services.rs's DOCUMENT_TYPES).
# A preferred application someone already chose stays.
signature=application/x-vnd.storytold-WordCraft
app=$(findpaths -p "$0" B_FIND_PATH_APPS_DIRECTORY WordCraft 2>/dev/null)
[ -n "$app" ] && [ -e "$app" ] && mimeset -a -f "$app"
for type in \
		application/vnd.openxmlformats-officedocument.wordprocessingml.document \
		application/vnd.openxmlformats-officedocument.wordprocessingml.template \
		application/vnd.ms-word.document.macroEnabled.12 \
		application/msword \
		application/rtf \
		text/rtf \
		application/vnd.oasis.opendocument.text; do
	setmime -dump "$type" 2>/dev/null | grep -q -- -preferredAppSig \
		|| setmime -set "$type" -preferredAppSig "$signature"
done
exit 0
