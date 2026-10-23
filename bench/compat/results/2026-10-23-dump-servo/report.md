# Text mode on the corpus: `sig --dump` with servo (2026-10-23)

Revision fab3f2a, timeout 30 s, width 80, 40 sites, one at a time, private D-Bus, no display. Text in `text/<id>.txt`.

Classes: OK = a heading and 20 words, or 60 words; THIN = 5 to 59 words; EMPTY = under 5 words; ERROR = sig failed; TIMEOUT = no page text in time. Words are counted in the printed text before the reference list.

Summary: OK 37, THIN 1, EMPTY 1, ERROR 0, TIMEOUT 1

| Site | Class | Exit | Time s | Out bytes | Words | Headings | Links | Controls | First line |
|---|---|---|---|---|---|---|---|---|---|
| example | OK | 0 | 5.6 | 1359 | 128 | 0 | 1 | 0 | This domain is for use in documentation examples without needing permission. |
| github-servo | OK | 0 | 3.4 | 1415 | 58 | 2 | 18 | 8 | Skip to content[1] |
| gitlab | OK | 0 | 2.8 | 12606 | 1016 | 33 | 105 | 12 | GitLab - Speed with control for agentic software engineering |
| chatgpt | TIMEOUT | -9 | 60.1 | 1516 | 122 | 3 | 11 | 13 | [ Log in ] [ Sign up for free ] |
| google-search | THIN | 0 | 2.7 | 1049 | 53 | 0 | 1 | 1 | ──────────────────────────────────────────────────────────────────────────────── |
| duckduckgo | OK | 0 | 3.0 | 13406 | 1063 | 25 | 103 | 52 | DuckDuckGo[1] |
| youtube | OK | 0 | 3.2 | 3076 | 302 | 2 | 9 | 8 | [ Guide ] |
| twitch | OK | 0 | 3.4 | 8337 | 609 | 15 | 82 | 21 | [image: Cookies] |
| netflix | OK | 0 | 3.0 | 5157 | 576 | 16 | 23 | 26 | Netflix |
| wikipedia | OK | 0 | 2.5 | 2260 | 79 | 2 | 21 | 11 | Jump to content[1] |
| mdn-grid | OK | 0 | 2.2 | 28502 | 1569 | 19 | 294 | 46 | • Skip to main content[1] |
| docsrs | OK | 0 | 2.2 | 7717 | 653 | 12 | 59 | 3 | Docs.rs[1] |
| crates | OK | 0 | 2.0 | 9369 | 777 | 11 | 85 | 4 | [image] crates.io[1] |
| w3schools | OK | 0 | 2.1 | 17844 | 1288 | 32 | 163 | 42 | [1] |
| codepen | OK | 0 | 4.1 | 7319 | 639 | 11 | 56 | 9 | CodePen Home[1] |
| caniuse | OK | 0 | 4.2 | 5007 | 460 | 32 | 30 | 17 | • Home[1] |
| rustlang | OK | 0 | 2.2 | 5464 | 543 | 22 | 35 | 2 | [image: Rust Logo][1] |
| reddit-rust | OK | 0 | 3.8 | 18262 | 607 | 15 | 49 | 30 | Skip to main content[1] |
| x | OK | 0 | 2.9 | 1628 | 122 | 1 | 15 | 9 | Happening now. |
| linkedin | OK | 0 | 2.2 | 12724 | 548 | 16 | 104 | 6 | Skip to main content[1] |
| instagram | OK | 0 | 3.7 | 2272 | 316 | 8 | 3 | 15 | [image: Language] Switch Display Language [English ▾] |
| stackoverflow | OK | 0 | 4.3 | 24930 | 1831 | 25 | 175 | 11 | Skip to main content[1] |
| hn | OK | 0 | 3.3 | 17854 | 851 | 0 | 228 | 1 | [image][1] Hacker News[2]new[3] \| past[4] \| comments[5] \| ask[6] \| show[7] \| |
| bbc | OK | 0 | 2.1 | 19887 | 1931 | 57 | 114 | 11 | Skip to content[1] |
| nytimes | EMPTY | 0 | 7.3 | 0 | 0 | 0 | 0 | 0 |  |
| guardian | OK | 0 | 2.6 | 44494 | 3677 | 152 | 171 | 5 | Skip to main content[1]Skip to navigation[2] |
| medium | OK | 0 | 2.5 | 1567 | 65 | 2 | 18 | 3 | [1] |
| amazon | OK | 0 | 3.7 | 32189 | 803 | 50 | 143 | 13 | Skip to |
| steam | OK | 0 | 2.5 | 18190 | 1311 | 8 | 172 | 15 | [image: Link to the Steam Homepage][1] |
| apple | OK | 0 | 2.1 | 81749 | 12092 | 22 | 93 | 17 | Välj ett annat land eller område för att se det innehåll som finns där du är och |
| microsoft | OK | 0 | 3.5 | 11206 | 707 | 19 | 89 | 10 | Gå till huvudinnehåll[1] |
| cloudflare | OK | 0 | 3.2 | 10126 | 1035 | 23 | 60 | 26 | [image: Cloudflare wordmark][1] |
| gmail | OK | 0 | 2.9 | 743 | 32 | 1 | 4 | 4 | Sign in |
| gmaps | OK | 0 | 2.3 | 2410 | 217 | 1 | 5 | 3 | [ Språk: Svenska ] |
| osm | OK | 0 | 2.2 | 2707 | 150 | 2 | 31 | 8 | [image: OpenStreetMap logo] OpenStreetMap[1] |
| discord | OK | 0 | 2.4 | 5697 | 584 | 13 | 45 | 13 | Skip to main content[1][image: Home][image][image][image][2] |
| notion | OK | 0 | 2.5 | 6573 | 442 | 13 | 74 | 6 | [1] |
| figma | OK | 0 | 2.5 | 8095 | 635 | 17 | 82 | 13 | This website uses cookies, pixel tags, and local storage for performance, |
| spotify | OK | 0 | 2.9 | 2465 | 219 | 4 | 4 | 28 | Skip to main content[1] |
| archive | OK | 0 | 5.3 | 2156 | 193 | 0 | 21 | 10 | [ Close banner ] |
