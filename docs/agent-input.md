# Background agent input

This fork exposes independent Wayland input seats for background computer-use
agents while preserving the physical/user seat.

## Seat model

Agent seats are opt-in to preserve compatibility with generic Wayland tools
that blindly bind the last advertised `wl_seat`. Start niri with, for example,
`NIRI_AGENT_SEATS=2` to advertise:

- `niri-agent-1`
- `niri-agent-2`

`NIRI_AGENT_SEATS` accepts `0..8`; the default is `0`.
Each agent seat owns a distinct Smithay pointer and keyboard handle, so pointer
location, pointer focus, keyboard focus and modifier delivery are independent
from the primary seat.

The agent seats intentionally do not drive niri key bindings, focus-follows-mouse,
hot corners, overview gestures, physical keyboard LEDs, the user's cursor
renderer, or layout/workspace activation.

## Standard virtual input protocols

The existing protocol globals remain the transport:

- `zwlr_virtual_pointer_manager_v1` / `zwlr_virtual_pointer_v1` version 2
- `zwp_virtual_keyboard_manager_v1` / `zwp_virtual_keyboard_v1` version 1

A virtual pointer created with one of the `niri-agent-*` seats is dispatched
through that seat instead of the primary input pipeline. Relative motion,
absolute motion, buttons, axis source/stop/discrete and frame semantics remain
the standard wlroots protocol semantics.

Smithay's virtual-keyboard implementation already binds every virtual keyboard
to the `wl_seat` passed to `create_virtual_keyboard`; giving an agent seat
keyboard focus therefore makes standard `zwp_virtual_keyboard_v1` keymap,
key and modifier requests land only on that seat's target.

## Background foreign-toplevel activation

`zwlr_foreign_toplevel_handle_v1.activate(seat)` now preserves the supplied
seat. With the primary seat it keeps the normal niri behavior. With an agent
seat it sets only that seat's keyboard focus and does not raise the window,
switch workspaces or change the user's layout focus.

For pointer delivery, the activation target is bound to the concrete
client-side `wl_seat` resource that issued the request. Subsequent virtual
pointer motion from that same resource is hit-tested directly against the
selected toplevel's tile, independently of the user's stacking order. This
allows an occluded target (and a target on a non-active workspace of the same
output) to receive pointer focus without visually raising or switching to it.
A different Wayland connection starts with no pointer target, so explicit
desktop-pointer operations do not inherit a stale window selection.

For clients that require activation read-back, niri sends an `Activated`
state acknowledgement only to the foreign-toplevel handle that requested the
isolated activation. Global foreign-toplevel state continues to describe the
primary user focus, avoiding false focus changes in panels and task switchers.

All agent focus is cleared when session locking begins. Agent pointer events
are dropped and isolated activation is refused until the session is fully
unlocked.

## CUA compatibility

Current `trycua/cua` generic Wayland input already:

1. enumerates `wl_seat` globals,
2. activates a target through wlr foreign-toplevel using the selected seat,
3. creates `zwlr_virtual_pointer_v1` and `zwp_virtual_keyboard_v1` objects
   for that same seat.

Its seat selector excludes the Hyprland-plugin names `Cua-Agent` and
`Cua-Agent-2`, but preserves the last advertised ordinary seat. Upstream
`wtype` likewise overwrites its seat binding for every advertised `wl_seat`,
so it ends up using the last seat. The companion CUA compatibility patch recognizes `niri-agent-*` by name and
intentionally selects an agent seat instead of relying on registry ordering.
CUA's current `wtype` fallback still binds the last advertised seat; with
Agent mode explicitly enabled, niri's agent seats are advertised after the
primary seat, so that fallback lands on the same final agent lane.

One limitation remains in current upstream CUA: its generic Wayland selector
always chooses one "selected" seat, so multiple simultaneous CUA sessions do
not yet distribute themselves across `niri-agent-1` and `niri-agent-2`.
Native Wayland clients can select either seat today. Full multi-CUA lane
assignment needs a small CUA-side seat-selection change (for example,
session/cursor-id -> `niri-agent-N`), while this compositor side is already
multi-seat.
