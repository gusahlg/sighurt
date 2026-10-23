# Text mode on the corpus: `sig --dump` with servo (2026-10-23)

Revision fab3f2a, timeout 30 s, width 80, 4 sites, one at a time, private D-Bus, no display. Text in `text/<id>.txt`.

Classes: OK = a heading and 20 words, or 60 words; THIN = 5 to 59 words; EMPTY = under 5 words; ERROR = sig failed; TIMEOUT = no page text in time. Words are counted in the printed text before the reference list.

Summary: OK 4, THIN 0, EMPTY 0, ERROR 0, TIMEOUT 0

| Site | Class | Exit | Time s | Out bytes | Words | Headings | Links | Controls | First line |
|---|---|---|---|---|---|---|---|---|---|
| github-servo | OK | 0 | 3.4 | 13993 | 906 | 30 | 115 | 15 | Skip to content[1] |
| wikipedia | OK | 0 | 2.8 | 55554 | 2051 | 12 | 486 | 22 | Jump to content[1] |
| reddit-rust | OK | 0 | 3.8 | 19412 | 634 | 15 | 50 | 30 | Skip to main content[1] |
| archive | OK | 0 | 5.1 | 2156 | 193 | 0 | 21 | 10 | [ Close banner ] |
