# Chat example (standard library items used as std)

An offline capable realtime chat written in the Ostrel programming language.
Source: `chat.ostl`. Companion: `../chat-nostd/` replaces every standard library
convenience that is not yet proven generic by app code (D30 strict reading).

## Scope

Included:

* PASETO sign in with unique handles (std auth, open registration)
* rooms: create, join, leave; all rooms and their member lists are visible to every signed in user
* live messages in a total order; the newest 200 messages per room are shown and synced
* offline send with a pending marker, offline reload through the app shell
* persistence across reload, sync status
* own messages right aligned with custom CSS

Not included: typing indicators, unread counts, editing or deleting messages
(denied by default), private rooms, moderation. Any member can read the full
history of a room they join.

## Standard library items used

`split`, `entry`, `selected:`, `scroll: end`, `look:`, the theme's pending and
rejected styling, `Time.clock`, base elements, std auth and `User`.

## Status

* The compiler cannot parse this file yet. A parse check is added once the
  parser lands.
* Line count: 48 non blank lines by hand. This is not an official number. The
  official count comes only from `tools/loc` after `ostrel fmt` on a tagged
  commit, together with the D30 ruling per std item.
