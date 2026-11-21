# Engage console

The console is a team work surface. Channels occupy a compact left rail, a middle list shows conversations or tasks, and the selected thread holds the main workspace. On narrow screens these panes become sequential views with explicit back navigation.

Use a system text stack for an application that works beside terminals and native tools. Titles use the same family at 18–24px, medium weight; body text uses 14px with 1.6 line height. Code and identifiers use the system monospace stack. Message text has a 72-character measure.

Define light and dark color tokens in the stylesheet. Light mode uses warm white backgrounds, ink text, restrained gray boundaries, and a forest-green action accent. Dark mode follows the operating system and uses dark neutral surfaces with lighter green actions. State is communicated in text as well as color.

Buttons, inputs, and menus use 8px radii and consistent 36px minimum heights. Shared form, toolbar, empty-state, and collection components own their complete interaction layout. A 44px target is used for compact mobile navigation. Borders separate panes; cards are reserved for thread entries and do not wrap other cards.

Conversation and task controls stay near the object they affect. Composer actions align below the text field. Settings use a left navigation and one editor surface. Empty states expose the relevant Create action without explanatory filler. Errors remain visible until dismissed or resolved. Focus rings, selection, caret, scrollbar colors, disabled states, and reduced-motion behavior use the same tokens.

Motion is limited to the thread pane appearing over 120ms. Live updates preserve the current draft and focus. Markdown is sanitized before rendering; identifiers and JSON use escaped text.
