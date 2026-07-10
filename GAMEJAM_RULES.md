# raylib 6.x gamejam — Rules

> Extracted from the official itch.io jam page (saved copy: `raylib 6.x gamejam - itch.io.html`).
> Hosted by Ray (raysan5) — hashtag `#raylibgamejam`.

## Dates

- **Start / submissions open:** July 6th 2026, 18:00 UTC
- **Submissions due:** July 12th 2026, 18:00 UTC (6 days)
- **Voting/rating ends:** July 18th 2026, 18:00 UTC (6 days after the jam ends)

## Theme

**hex + merge** *(announced at jam start; the two most-voted themes on X, shown as an image on the jam page)*

> The themes have been voted on X!!! AND THE TWO MOST VOTED THEMES that DEFINE the OBJECTIVE of this GAMEJAM are... **hex + merge**

## Goal

The goal of this gamejam is making a game with raylib in 6 DAYS with some technical
constraints and following proposed theme. One of the requirements is that game **must run
on web (wasm)**. A GitHub raylib game template for this gamejam is available, everyone is
free to use it — it automates game building (Windows, Linux, WebAssembly) with GitHub
Actions on every commit.

**WARNING! raylib is for ADVENTURERS!** raylib is a C programming library, no visual editor
or high-level engine features, just coding in the most pure spartan-programmers way.

## Constraints

**Hard constraints (mandatory requirements):**
- Game **MUST be 720x720 pixels resolution!**
- Game **MUST be run on Web** with WebAssembly technology
- Game **MUST be under 64MB (full package: .wasm + .data)** — the submission page clarifies: game resources package (`.data`) MUST be under 64MB

**Soft constraints (recommended features):**
- Game should use **provided raylib template**
- Game should be playable with **Keyboard/Mouse and Touch**
- Game should be **open source** and available in GitHub

## Rules

- An itch.io account is required to join the jam and submit your entry.
- Team submissions are acceptable for teams of 4 members or less. (Prizes are awarded for each submission, not the individual contributors)
- About the Assets used in your game: you must own the copyright to, or be granted a license from the original copyright holder to redistribute any assets that you ship with your game (including but not limited to: Images, Sounds or Music, Fonts, Code, or any other artistic works). If in doubt of the copyright status of an asset, ask the original copyright holder. A list of royalty-free assets is provided in the Resources section.
- Game must not contain illegal, hateful, derogatory, NSFW or bigoted content. No racism, sexism, or any other form of discrimination — the game will be disqualified.
- Game must be created and submitted within the gamejam time frame.
- No late entries will be accepted.

## Rating

5-star system in three categories: **Theme** (how well the theme is used), **Fun** (is the game enjoyable), **Polish** (overall quality / time well spent).

Hybrid votes model:
- **Public votes** (first round): open to ANY itch.io user — everybody can play and rate the web games. Highest-rated games pass to the jury round.
- **Jury votes** (second round): a panel of experienced developers (disclosed when the jam ends) privately rates the top public-voted games and decides the winners. The jury also compensates for double/unfair public voting.

Rating period: 6 days after the jam ends. Leaving written feedback with ratings is encouraged.

## Prizes

- Prizes for 3 games; one entry can only win in one category → 3 winners. Honorable mentions possible.
- Prizes subject to availability per country (some countries don't support Steam gift cards).
- **ALL participants that submit a game following the constraints receive an itch.io key for one raylib technologies tool** (request from Ray on the raylib Discord after submission).

## FAQ (selected)

- **AI usage:** not forbidden ("I can't control that and I can't avoid AI usage... but what is the point of participating in a technical gamedev challenge with a low-level C library if you decide to delegate most of that work to an AI system?").
- **Smaller render, scaled?** Yes — render to a smaller square RenderTexture and scale (multiples of 720 recommended: 360x360, 180x180).
- **Must it run on Web?** Yes — compiled to WebAssembly and uploaded to itch.io for evaluation. Other platform builds may be uploaded too but won't be evaluated.
- **Pre-existing own material:** allowed, but the spirit is making something new, and the game MUST follow the theme disclosed at jam start.
- **Third-party assets:** free/public-domain art & audio allowed (check licenses). Teams up to 4.
- **Updates during voting:** not possible once the jam is finished.
- **Community:** #raylib-gamejam channel on the raylib Discord.

## Legal

Game creators retain all rights of the gamejam submitted game.

---

## Analysis: is SpacetimeDB allowed? *(our notes, not official rules)*

**Yes — nothing forbids external libraries, networking, servers or databases.** They are simply not addressed by the rules. What must hold: game made with raylib, 720x720, runs on Web/WASM on itch.io, package ≤ 64MB. Practical caveats:

1. The SpacetimeDB server must be independently hosted and stay up through the jam **and the 6-day voting window**.
2. itch.io serves over HTTPS → client must use `wss://` (TLS in front of SpacetimeDB).
3. The client SDK compiles into the wasm bundle (counts toward 64MB — not a concern at this size).
4. **Technical risk:** raylib web = emscripten target; spacetimedb-sdk browser support targets `wasm32-unknown-unknown`. Compatibility of both in one build is unverified.
5. Game should remain playable solo so it stays ratable if the server is empty/down.
