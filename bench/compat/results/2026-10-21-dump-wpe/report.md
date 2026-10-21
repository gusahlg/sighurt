# Text mode on the corpus: `sig --dump` with WPE WebKit (2026-10-21)

Revision 25cf0ac, timeout 30 s, width 80, 40 sites, one at a time, private D-Bus, no display. Text in `text/<id>.txt`.

Classes: OK = a heading and 20 words, or 60 words; THIN = 5 to 59 words; EMPTY = under 5 words; ERROR = sig failed; TIMEOUT = no page text in time. Words are counted in the printed text before the reference list.

Summary: OK 35, THIN 2, EMPTY 2, ERROR 1, TIMEOUT 0

| Site | Class | Exit | Time s | Out bytes | Words | Headings | Links | Controls | First line |
|---|---|---|---|---|---|---|---|---|---|
| example | OK | 0 | 2.2 | 1359 | 128 | 0 | 1 | 0 | This domain is for use in documentation examples without needing permission. |
| github-servo | OK | 0 | 2.9 | 12229 | 921 | 29 | 115 | 12 | Skip to content[1] |
| gitlab | OK | 0 | 3.0 | 12411 | 998 | 33 | 102 | 7 | GitLab - Speed with control for agentic software engineering |
| chatgpt | OK | 0 | 3.2 | 1616 | 133 | 3 | 11 | 14 | [ Open sidebar ] |
| google-search | THIN | 0 | 3.1 | 1050 | 55 | 0 | 1 | 1 | ──────────────────────────────────────────────────────────────────────────────── |
| duckduckgo | OK | 0 | 3.3 | 11944 | 1001 | 23 | 85 | 50 | [ All Results ] DuckDuckGo[1] |
| youtube | EMPTY | 0 | 7.9 | 75 | 3 | 0 | 1 | 2 | [ Guide ] |
| twitch | OK | 0 | 3.1 | 2789 | 335 | 1 | 15 | 19 | Cookies and Advertising Choices |
| netflix | OK | 0 | 3.4 | 3402 | 303 | 16 | 23 | 26 | Netflix |
| wikipedia | OK | 0 | 2.9 | 54612 | 1974 | 11 | 481 | 5 | Jump to content[1] |
| mdn-grid | OK | 0 | 2.7 | 26832 | 1608 | 19 | 280 | 39 | • Skip to main content[1] |
| docsrs | OK | 0 | 2.7 | 7695 | 644 | 13 | 60 | 2 | [1] |
| crates | OK | 0 | 2.7 | 9306 | 749 | 11 | 85 | 4 | [image] crates.io[1] |
| w3schools | OK | 0 | 3.0 | 16536 | 1133 | 31 | 160 | 37 | [1] [ Menu ] |
| codepen | OK | 0 | 3.6 | 6396 | 599 | 10 | 47 | 9 | CodePen Home[1] |
| caniuse | OK | 0 | 6.3 | 4876 | 447 | 32 | 29 | 17 | • Home[1] |
| rustlang | OK | 0 | 2.6 | 5464 | 543 | 22 | 35 | 2 | [image: Rust Logo][1] |
| reddit-rust | OK | 0 | 4.0 | 15087 | 313 | 9 | 40 | 27 | Skip to main content[1] |
| x | OK | 0 | 2.9 | 990 | 30 | 1 | 13 | 0 | [image: X] |
| linkedin | OK | 0 | 6.8 | 12709 | 545 | 16 | 104 | 8 | Skip to main content[1] |
| instagram | OK | 0 | 9.7 | 2272 | 316 | 8 | 3 | 15 | [image: Language] Switch Display Language [English ▾] |
| stackoverflow | OK | 0 | 5.6 | 23121 | 1768 | 25 | 155 | 9 | Skip to main content[1] |
| hn | OK | 0 | 3.3 | 17932 | 848 | 0 | 227 | 1 | [image][1] Hacker News[2]new[3] \| past[4] \| comments[5] \| ask[6] \| show[7] \| |
| bbc | OK | 0 | 2.7 | 16590 | 1587 | 57 | 88 | 9 | Skip to content[1] |
| nytimes | EMPTY | 0 | 7.5 | 0 | 0 | 0 | 0 | 0 |  |
| guardian | OK | 0 | 2.8 | 55703 | 4356 | 194 | 270 | 6 | Skip to main content[1]Skip to navigation[2] |
| medium | OK | 0 | 2.8 | 742 | 40 | 2 | 7 | 3 | [1] |
| amazon | OK | 0 | 4.5 | 32189 | 803 | 50 | 143 | 13 | Skip to |
| steam | OK | 0 | 3.5 | 15414 | 1117 | 7 | 140 | 10 | Sign in[1] Store[2] Community[3] About[4] Support[5] |
| apple | OK | 0 | 3.7 | 78271 | 11937 | 23 | 39 | 18 | Välj ett annat land eller område för att se det innehåll som finns där du är och |
| microsoft | OK | 0 | 4.3 | 10670 | 690 | 19 | 80 | 9 | Gå till huvudinnehåll[1] |
| cloudflare | OK | 0 | 4.0 | 9796 | 1018 | 23 | 56 | 25 | [1] |
| gmail | THIN | 0 | 8.4 | 580 | 17 | 1 | 3 | 4 | Sign in |
| gmaps | OK | 0 | 3.2 | 2410 | 217 | 1 | 5 | 3 | [image: Google] |
| osm | OK | 0 | 8.3 | 916 | 23 | 1 | 11 | 3 | [image: OpenStreetMap logo] OpenStreetMap[1] [  ] [ Main ] |
| discord | OK | 0 | 6.0 | 4285 | 433 | 9 | 35 | 12 | Skip to main content[1][image: Home][image][image][image][2] |
| notion | OK | 0 | 4.0 | 5681 | 428 | 14 | 52 | 7 | [1] |
| figma | OK | 0 | 3.7 | 8485 | 712 | 17 | 79 | 10 | This website uses cookies, pixel tags, and local storage for performance, |
| spotify | OK | 0 | 5.8 | 16413 | 1071 | 8 | 130 | 133 | Skip to main content[1] |
| archive | ERROR | 1 | 10.5 | 0 | 0 | None | None | None |  |

## Before and after the page-grace change

Before: the text was taken 300 ms after the load ended. After: not before 1.5 s after the load ended, and 6 s when the snapshot holds under 30 words (`frontends/sighurt-tui/src/dump.rs`: GRACE, GRACE_THIN).

| Site | Before | After | Words before | Words after |
|---|---|---|---|---|
| google-search | OK | THIN | 223 | 55 |
| duckduckgo | OK | OK | 2171 | 1001 |
| twitch | THIN | OK | 27 | 335 |
| crates | EMPTY | OK | 0 | 749 |
| stackoverflow | EMPTY | OK | 0 | 1768 |
| medium | THIN | OK | 19 | 40 |
| amazon | OK | OK | 498 | 803 |
| spotify | THIN | OK | 32 | 1071 |

Before: OK 31, THIN 4, EMPTY 4, ERROR 1, TIMEOUT 0
After: OK 35, THIN 2, EMPTY 2, ERROR 1, TIMEOUT 0

## Remaining

- youtube: 281 nodes but 2 words; the feed is not in the page the headless session gets (not investigated further).
- nytimes: the document is empty (2 nodes); the site serves nothing to this client.
- gmail: the sign-in page, 17 words, is what the page has.
- archive: `Operation was cancelled`, the navigation ends before a document commits; a WPE load issue, not text mode.
- google-search: 55 words here against 223 in the first run; the page differs between runs (consent/language).
