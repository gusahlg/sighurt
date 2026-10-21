# Text mode on the corpus: `sig --dump` with WPE WebKit (2026-10-21)

Revision 25cf0ac, timeout 30 s, width 80, 40 sites, one at a time, private D-Bus, no display. Text in `text/<id>.txt`.

Classes: OK = a heading and 20 words, or 60 words; THIN = 5 to 59 words; EMPTY = under 5 words; ERROR = sig failed; TIMEOUT = no page text in time. Words are counted in the printed text before the reference list.

Summary: OK 31, THIN 4, EMPTY 4, ERROR 1, TIMEOUT 0

| Site | Class | Exit | Time s | Out bytes | Words | Headings | Links | Controls | First line |
|---|---|---|---|---|---|---|---|---|---|
| example | OK | 0 | 4.6 | 1359 | 128 | 0 | 1 | 0 | This domain is for use in documentation examples without needing permission. |
| github-servo | OK | 0 | 2.3 | 9721 | 784 | 30 | 80 | 10 | Skip to content[1] |
| gitlab | OK | 0 | 6.4 | 11922 | 935 | 32 | 101 | 4 | Get free trial[1][2] [ Main Menu ] |
| chatgpt | OK | 0 | 2.1 | 1586 | 130 | 3 | 11 | 13 | [ Open sidebar ] |
| google-search | OK | 0 | 2.1 | 2149 | 223 | 1 | 3 | 5 | [image: Google] |
| duckduckgo | OK | 0 | 3.6 | 149914 | 2171 | 51 | 176 | 57 | [ All Results ] DuckDuckGo[1] |
| youtube | EMPTY | 0 | 2.8 | 75 | 3 | 0 | 1 | 2 | [ Guide ] |
| twitch | THIN | 0 | 2.0 | 299 | 27 | 0 | 2 | 9 | [ Skip to...AltP ] |
| netflix | OK | 0 | 2.9 | 3402 | 303 | 16 | 23 | 26 | Netflix |
| wikipedia | OK | 0 | 2.0 | 54612 | 1974 | 11 | 481 | 5 | Jump to content[1] |
| mdn-grid | OK | 0 | 1.7 | 26832 | 1608 | 19 | 280 | 39 | • Skip to main content[1] |
| docsrs | OK | 0 | 1.6 | 7695 | 644 | 13 | 60 | 2 | [1] |
| crates | EMPTY | 0 | 1.5 | 0 | 0 | 0 | 0 | 0 |  |
| w3schools | OK | 0 | 2.2 | 16536 | 1133 | 31 | 160 | 37 | [1] [ Menu ] |
| codepen | OK | 0 | 5.3 | 6394 | 599 | 10 | 47 | 9 | CodePen Home[1] |
| caniuse | OK | 0 | 9.3 | 4876 | 447 | 32 | 29 | 17 | • Home[1] |
| rustlang | OK | 0 | 1.4 | 5464 | 543 | 22 | 35 | 2 | [image: Rust Logo][1] |
| reddit-rust | OK | 0 | 2.8 | 15866 | 329 | 9 | 41 | 26 | Skip to main content[1] |
| x | OK | 0 | 4.8 | 990 | 30 | 1 | 13 | 0 | [image: X] |
| linkedin | OK | 0 | 5.5 | 12272 | 520 | 16 | 101 | 7 | Skip to main content[1] |
| instagram | OK | 0 | 3.0 | 2272 | 316 | 8 | 3 | 15 | [image: Language] Switch Display Language [English ▾] |
| stackoverflow | EMPTY | 0 | 1.3 | 0 | 0 | 0 | 0 | 0 |  |
| hn | OK | 0 | 2.6 | 17969 | 845 | 0 | 227 | 1 | [image][1] Hacker News[2]new[3] \| past[4] \| comments[5] \| ask[6] \| show[7] \| |
| bbc | OK | 0 | 1.6 | 16640 | 1603 | 57 | 88 | 9 | Skip to content[1] |
| nytimes | EMPTY | 0 | 1.6 | 0 | 0 | 0 | 0 | 0 |  |
| guardian | OK | 0 | 1.8 | 55690 | 4356 | 194 | 270 | 6 | Skip to main content[1]Skip to navigation[2] |
| medium | THIN | 0 | 1.7 | 549 | 19 | 2 | 6 | 2 | [1] |
| amazon | OK | 0 | 3.0 | 9918 | 498 | 37 | 81 | 13 | Skip to |
| steam | OK | 0 | 2.3 | 15364 | 1115 | 7 | 140 | 10 | Sign in[1] Store[2] Community[3] About[4] Support[5] |
| apple | OK | 0 | 2.1 | 78271 | 11937 | 23 | 39 | 18 | Välj ett annat land eller område för att se det innehåll som finns där du är och |
| microsoft | OK | 0 | 2.6 | 10670 | 690 | 19 | 80 | 9 | Gå till huvudinnehåll[1] |
| cloudflare | OK | 0 | 2.3 | 9796 | 1018 | 23 | 56 | 25 | [1] |
| gmail | THIN | 0 | 2.4 | 580 | 17 | 1 | 3 | 4 | Sign in |
| gmaps | OK | 0 | 1.8 | 2410 | 217 | 1 | 5 | 3 | [image: Google] |
| osm | OK | 0 | 2.0 | 916 | 23 | 1 | 11 | 3 | [image: OpenStreetMap logo] OpenStreetMap[1] [  ] [ Main ] |
| discord | OK | 0 | 3.2 | 4285 | 433 | 9 | 35 | 12 | Skip to main content[1][image: Home][image][image][image][2] |
| notion | OK | 0 | 1.8 | 5271 | 385 | 13 | 50 | 3 | [1] |
| figma | OK | 0 | 2.2 | 8165 | 667 | 17 | 79 | 5 | Skip to main content[1] |
| spotify | THIN | 0 | 2.6 | 612 | 32 | 0 | 3 | 9 | Skip to main content[1] |
| archive | ERROR | 1 | 10.4 | 0 | 0 | None | None | None |  |
