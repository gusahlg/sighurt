# Text mode on the corpus: `sig --dump` with WPE WebKit (2026-10-21)

Revision f730623, timeout 30 s, width 80, 40 sites, one at a time, private D-Bus, no display. Text in `text/<id>.txt`.

Classes: OK = a heading and 20 words, or 60 words; THIN = 5 to 59 words; EMPTY = under 5 words; ERROR = sig failed; TIMEOUT = no page text in time. Words are counted in the printed text before the reference list.

Summary: OK 38, THIN 1, EMPTY 1, ERROR 0, TIMEOUT 0

| Site | Class | Exit | Time s | Out bytes | Words | Headings | Links | Controls | First line |
|---|---|---|---|---|---|---|---|---|---|
| example | OK | 0 | 2.6 | 1359 | 128 | 0 | 1 | 0 | This domain is for use in documentation examples without needing permission. |
| github-servo | OK | 0 | 8.0 | 13963 | 902 | 30 | 115 | 14 | Skip to content[1] |
| gitlab | OK | 0 | 9.4 | 12616 | 1015 | 33 | 105 | 12 | GitLab - Speed with control for agentic software engineering |
| chatgpt | OK | 0 | 4.3 | 1546 | 123 | 3 | 11 | 14 | [ Log in ] [ Sign up for free ] |
| google-search | OK | 0 | 4.6 | 2149 | 223 | 1 | 3 | 5 | [image: Google] |
| duckduckgo | OK | 0 | 5.2 | 157467 | 2029 | 47 | 183 | 44 | DuckDuckGo[1] |
| youtube | OK | 0 | 10.2 | 3076 | 302 | 2 | 9 | 8 | [ Guide ] |
| twitch | OK | 0 | 5.6 | 1621 | 206 | 0 | 5 | 15 | [image: Cookies] |
| netflix | OK | 0 | 10.0 | 3561 | 330 | 16 | 23 | 26 | Netflix |
| wikipedia | OK | 0 | 5.0 | 55554 | 2051 | 12 | 486 | 22 | Jump to content[1] |
| mdn-grid | OK | 0 | 3.6 | 42794 | 2042 | 19 | 429 | 46 | • Skip to main content[1] |
| docsrs | OK | 0 | 3.5 | 7717 | 653 | 12 | 59 | 3 | Docs.rs[1] |
| crates | OK | 0 | 3.0 | 9293 | 758 | 11 | 85 | 4 | [image] crates.io[1] |
| w3schools | OK | 0 | 4.3 | 17085 | 1182 | 32 | 163 | 42 | [1] |
| codepen | OK | 0 | 11.0 | 7332 | 644 | 11 | 56 | 9 | CodePen Home[1] |
| caniuse | OK | 0 | 5.9 | 5007 | 460 | 32 | 30 | 17 | • Home[1] |
| rustlang | OK | 0 | 3.5 | 5464 | 543 | 22 | 35 | 2 | [image: Rust Logo][1] |
| reddit-rust | OK | 0 | 8.6 | 59985 | 2589 | 19 | 178 | 107 | Skip to main content[1] |
| x | OK | 0 | 8.5 | 1430 | 61 | 1 | 18 | 2 | Happening now. |
| linkedin | OK | 0 | 3.6 | 12303 | 525 | 16 | 101 | 7 | Skip to main content[1] |
| instagram | OK | 0 | 3.9 | 2272 | 316 | 8 | 3 | 15 | [image: Language] Switch Display Language [English ▾] |
| stackoverflow | OK | 0 | 7.5 | 24789 | 1815 | 25 | 173 | 11 | Skip to main content[1] |
| hn | OK | 0 | 3.7 | 17857 | 844 | 0 | 227 | 1 | [image][1] Hacker News[2]new[3] \| past[4] \| comments[5] \| ask[6] \| show[7] \| |
| bbc | OK | 0 | 4.6 | 19819 | 1938 | 57 | 114 | 11 | Skip to content[1] |
| nytimes | EMPTY | 0 | 12.8 | 0 | 0 | 0 | 0 | 0 |  |
| guardian | OK | 0 | 3.3 | 57873 | 4627 | 194 | 253 | 5 | Skip to main content[1]Skip to navigation[2] |
| medium | OK | 0 | 3.4 | 1567 | 65 | 2 | 18 | 3 | [1] |
| amazon | OK | 0 | 11.3 | 39707 | 907 | 54 | 163 | 13 | Skip to |
| steam | OK | 0 | 4.2 | 18786 | 1389 | 8 | 178 | 15 | [image: Link to the Steam Homepage][1] |
| apple | OK | 0 | 4.4 | 83189 | 12179 | 23 | 114 | 17 | Välj ett annat land eller område för att se det innehåll som finns där du är och |
| microsoft | OK | 0 | 3.7 | 11198 | 705 | 19 | 89 | 11 | Gå till huvudinnehåll[1] |
| cloudflare | OK | 0 | 10.5 | 10126 | 1035 | 23 | 60 | 26 | [image: Cloudflare wordmark][1] |
| gmail | THIN | 0 | 8.9 | 580 | 17 | 1 | 3 | 4 | Sign in |
| gmaps | OK | 0 | 3.3 | 2410 | 217 | 1 | 5 | 3 | [ Språk: Svenska ] |
| osm | OK | 0 | 4.0 | 2707 | 150 | 2 | 31 | 8 | [image: OpenStreetMap logo] OpenStreetMap[1] |
| discord | OK | 0 | 12.2 | 5697 | 584 | 13 | 45 | 13 | Skip to main content[1][image: Home][image][image][image][2] |
| notion | OK | 0 | 3.9 | 5810 | 431 | 14 | 54 | 9 | [1] |
| figma | OK | 0 | 4.7 | 8691 | 723 | 17 | 82 | 13 | This website uses cookies, pixel tags, and local storage for performance, |
| spotify | OK | 0 | 5.5 | 4184 | 281 | 8 | 27 | 27 | Skip to main content[1] |
| archive | OK | 0 | 4.0 | 7493 | 565 | 23 | 65 | 7 | Skip to main content[1] |

## Change against 2026-10-21-dump-wpe

`--dump` now gives the page a 1280x800 area and takes the frames the engine offers. Pages that draw with animation frames (YouTube's consent dialog, archive.org's load) did nothing before, because no frame was ever taken. youtube EMPTY (3 words) to OK (302); archive ERROR (Operation was cancelled) to OK (565). nytimes stays EMPTY: its document has no nodes. gmail stays THIN: its sign-in page has 17 words. Time per site rose by a few seconds (a debug build paints each frame).
