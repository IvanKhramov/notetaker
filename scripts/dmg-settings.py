# dmgbuild settings for scripts/package-macos.sh.
# Window size and icon centres match scripts/dmg-background.py.
app = defines["app"]  # noqa: F821 - provided by dmgbuild
background = defines["background"]  # noqa: F821
format = "UDZO"
files = [app]
symlinks = {"Applications": "/Applications"}
icon_locations = {"notetaker.app": (170, 170), "Applications": (490, 170)}
window_rect = ((200, 160), (660, 420))
default_view = "icon-view"
icon_size = 96
text_size = 13
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
