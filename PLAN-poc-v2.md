# Plan : PoC v2 — salle sombre unique, caméra, lampe en cône, triangles + table de merge

## Contexte

Le PoC actuel ([client/src/bin/web.rs](client/src/bin/web.rs)) : canvas 720x720 sans caméra, deux salles (petite salle éclairée à gauche + couloir + grande salle sombre), lampe torche circulaire (rayon 150, tout-ou-rien), 8 hexagones ramassables générés client-side. Les positions multijoueur passent par SpacetimeDB (`user` table / reducer `set_pos`) — cette couche réseau ne change pas.

Nouvelles specs (itération gameplay, toujours un PoC) :
- **Une seule grande salle sombre** — la salle de gauche et le couloir disparaissent.
- **Salle plus grande que la fenêtre** → caméra 2D qui suit le joueur.
- **Sans lampe : on voit un tout petit peu** (halo faible autour du joueur) ; la lampe **augmente la portée**.
- **Lumière en cône** dans la direction de déplacement, avec **fade** (pas toute la map).
- **6 triangles** à ramasser en passant dessus, couleurs **vraiment aléatoires** (doublons possibles).
- **Table de merge** (touche pour ouvrir) : hexagone incomplet montrant quelle couleur va à quel emplacement ; assemblage par **drag & drop** depuis l'inventaire.
- Hexagone complet → **déverrouille la porte**.

Décisions utilisateur : drag & drop pour le placement ; couleurs avec doublons possibles ; triangles restent client-side (pas de synchro serveur).

## Décisions de design

1. **Éclairage par lightmap multipliée** (au lieu des draws conditionnels actuels). Une `RenderTexture2D` 720x720 : on la clear à une couleur ambiante quasi-noire (≈ `(10,10,14)` — le « on voit un tout petit peu » global), on y dessine les lumières en blending additif (dans le même `Camera2D` que le monde), puis on dessine la scène normalement et on applique la lightmap plein écran en `BLEND_MULTIPLIED`. Tout est dessiné (triangles, autres joueurs, porte) ; c'est l'obscurité qui masque — ça supprime toute la logique conditionnelle `DARK_ROOM`/`distance <= FLASHLIGHT_RADIUS` du rendu actuel, et le fade est gratuit. Pas de shader (overkill pour un PoC, et fragile sur WebGL/emscripten).
   - Gotcha connu : une render texture se dessine inversée verticalement → source rect à hauteur négative (`Rectangle::new(0,0,720,-720)`).

2. **Cône avec fade par secteurs empilés**. raylib n'a pas de « sector gradient » ; on empile N `draw_circle_sector` concentriques (N ≈ 10) de rayons croissants jusqu'à `FLASHLIGHT_RANGE`, tous à la même alpha faible, en additif : le centre accumule N couches, le bord une seule → dégradé radial dans le cône. Angle du cône centré sur `facing` (voir 3), demi-angle ≈ 28°. Évite rlgl bas niveau (vertex colors à la main).

3. **Direction du cône = dernière direction de déplacement**. Nouveau champ `facing: Vector2` dans `State`, mis à jour (normalisé) à chaque frame où l'input ZQSD est non nul, conservé quand le joueur s'arrête. Init `(1, 0)`.

4. **Layout cible = multiset des couleurs spawnées, permuté**. Au spawn, chaque triangle tire un `palette_index` aléatoire (doublons OK). Le layout cible de l'hexagone (`target_slots: [usize; 6]`) = les 6 indices des triangles spawnés, mélangés (Fisher-Yates avec le `Rng` xorshift existant). Ainsi l'hexagone demande exactement les couleurs qui existent sur la map — toujours complétable.

5. **Drag & drop avec hit-test point-dans-triangle**. Les 6 slots de l'hexagone sont des triangles (centre + 2 sommets adjacents) ; le drop est validé par un test point-dans-triangle (méthode des signes/produits vectoriels, ~10 lignes, pas de dépendance). Drop sur un slot vide de la bonne couleur → placé ; sinon retour à l'inventaire. Feedback live pendant le drag : contour du slot survolé en blanc si la couleur matche, en rouge sinon.

## Constantes / géométrie

- `ROOM: Rect = { x: 0, y: 0, w: 1200, h: 1200 }` — ~1.7× la fenêtre (720), « un peu plus grande ».
- `DOOR` : ouverture de ~100px au milieu du mur droit ; rect qui déborde vers l'extérieur pour que « passer la porte » soit détectable.
- `PALETTE` étendue à **6 couleurs** (ajout de `("yellow", Color::GOLD)` aux 5 existantes).
- `TRIANGLE_COUNT = 6` (remplace `RESOURCE_COUNT = 8`), triangles dessinés via `draw_poly(pos, 3, …)` (le `draw_poly(…, 6, …)` actuel dessinait des hexagones).
- Lumière : `AMBIENT_GLOW_RADIUS ≈ 90` (halo faible permanent, `draw_circle_gradient` additif), `FLASHLIGHT_RANGE ≈ 300`, `CONE_HALF_ANGLE ≈ 28°`, `CONE_FADE_STEPS ≈ 10`.

## Étapes d'implémentation

Tout se passe dans `client/src/bin/web.rs` sauf mention contraire. La couche réseau (FrameData, parse_user_row, apply_database_update, send throttling…) est intacte.

### Étape 1 — Monde unique + caméra
- Supprimer `LEFT_ROOM`, `CORRIDOR`, `DARK_ROOM`, `ROOMS` ; introduire `ROOM` et `DOOR`.
- `move_with_collision` : garder le déplacement axe-par-axe, mais le test devient « le cercle joueur tient dans `ROOM` — ou dans `ROOM ∪ DOOR` si la porte est déverrouillée ».
- `Camera2D { offset: (360, 360), target: player_pos, zoom: 1.0 }`, target clampé à `[360, ROOM.w - 360]` sur chaque axe pour que la vue ne sorte jamais de la salle.
- `render()` : le monde (sol de la salle, murs, triangles, porte, joueurs) se dessine dans `begin_mode2D(camera)` ; le HUD et la table de merge après, en screen space.
- Spawn local : centre de la salle `(600, 600)`.

### Étape 2 — Lightmap (ambiant + halo + cône)
- `State` gagne `lightmap: RenderTexture2D` (créée dans `main()` via `rl.load_render_texture(&thread, 720, 720)`) et `facing: Vector2`.
- Chaque frame, avant le rendu monde : `begin_texture_mode(lightmap)` → clear couleur ambiante → `begin_mode2D(camera)` + `begin_blend_mode(BLEND_ADDITIVE)` → halo (`draw_circle_gradient`, toujours) + cône (si `flashlight_on`, secteurs empilés — décision 2).
- Après le rendu monde : dessiner la lightmap plein écran en `BLEND_MULTIPLIED` (source rect hauteur négative).
- Supprimer l'ancien bloc « Flashlight glow + reveals » et les filtres `DARK_ROOM.contains_circle` sur les autres joueurs — tout est dessiné dans le monde, la lightmap masque.

### Étape 3 — Triangles : spawn, pickup, HUD
- `spawn_resources` → `spawn_triangles` : 6 positions aléatoires dans `ROOM` (marge 40px des murs ; rejection-sampling simple pour garder ≥ 120px entre triangles et vs le spawn joueur), `palette_index` aléatoire (doublons OK).
- Après le spawn, construire `target_slots: [usize; 6]` (décision 4).
- Pickup : distance joueur↔triangle ≤ rayons → `inventory.push(palette_index)`, `swap_remove` — **sans condition de lampe** (spec : « en passant dessus »). Remplace le champ `collected: [u32; 5]`.
- HUD : remplacer les compteurs par couleur par « Triangles : n/6 » + mini-icônes des triangles en inventaire, et les hints `ZQSD bouger · F lampe · E table de merge`.

### Étape 4 — Table de merge (drag & drop)
- Touche **E** (adjacente à ZQSD) toggle `merge_open: bool` ; mouvement désactivé tant que c'est ouvert (le heartbeat réseau continue).
- Panneau screen-space centré (~520x440, fond sombre semi-transparent) :
  - **Hexagone** (centre haut du panneau, rayon ≈ 110) : 6 wedges triangulaires. Slot vide = fill à ~25% d'alpha de la couleur requise + contour (on voit quelle couleur va où) ; slot rempli = fill pleine couleur.
  - **Inventaire** (rangée en bas) : un petit triangle (~22px) par item ramassé non placé.
- Drag & drop : mousedown sur un triangle d'inventaire → `dragging: Option<usize>`, le triangle suit la souris ; mouseup → hit-test des 6 wedges (décision 5) ; slot vide + bonne couleur → `placed[slot] = true`, retrait de l'inventaire ; sinon rien (retour visuel à l'inventaire). Feedback de survol pendant le drag.
- `placed.iter().all()` → `hexagon_complete = true` + message « Hexagone complet — porte déverrouillée » dans le panneau.

### Étape 5 — Porte + victoire
- Porte dessinée dans le mur droit : verrouillée = rectangle barré sombre/rougeâtre ; déverrouillée = ouverture verte + léger glow ajouté à la lightmap à sa position (repère de navigation).
- Déverrouillée, la collision s'ouvre (étape 1) ; quand le centre du joueur dépasse le mur → `escaped = true` → overlay centré « Échappé ! » et inputs de jeu figés.

### Étape 6 — Serveur + docs
- [server/src/lib.rs](server/src/lib.rs) : spawn `client_connected` passe de `(140, 360)` (centre de l'ex-salle gauche) à `(600, 600)` ; mettre à jour le commentaire. Nécessite un `./server/publish.sh`.
- Mettre à jour le doc-comment de module de `web.rs` et la section description/vérification de [WORK.md](WORK.md).

## Fichiers touchés

| Fichier | Changement |
|---|---|
| `client/src/bin/web.rs` | L'essentiel : monde/caméra, lightmap, triangles, merge UI, porte. |
| `server/src/lib.rs` | Position de spawn uniquement. |
| `WORK.md` | Description du PoC + étapes de vérification. |

Inchangés : `client/web/game.html`, `index.html`, tout le protocole SpacetimeDB côté client, `build-web.sh`.

## Vérification

Build & run comme documenté dans WORK.md : `spacetime start` + `./server/publish.sh` (spawn modifié) + `./build-web.sh` + `python3 -m http.server -d client/web 8080`.

1. **Caméra** : la salle dépasse la fenêtre ; en marchant, la caméra suit puis se clampe aux murs (pas de zone hors-salle visible).
2. **Lumière** : sans lampe, seulement un petit halo autour du joueur, le reste quasi-noir ; `F` → cône qui pointe dans la direction ZQSD courante, fade progressif, portée bien inférieure à la taille de la salle ; le cône pivote quand on change de direction et garde la dernière direction à l'arrêt.
3. **Triangles** : passer dessus les ramasse (avec ou sans lampe), le HUD passe à n/6.
4. **Table de merge** : `E` ouvre/ferme ; l'hexagone montre 6 slots teintés des couleurs requises (doublons possibles) ; drag & drop d'un triangle sur un slot de la bonne couleur → placé ; mauvaise couleur ou slot occupé → refusé (feedback rouge au survol) ; 6/6 → message de complétion.
5. **Porte** : verrouillée avant complétion (bloque le passage) ; après → visuel ouvert, on passe au travers → overlay « Échappé ! ».
6. **Multijoueur intact** : `index.html` en deux panes — les deux joueurs se voient bouger quand ils sont dans la portée lumineuse l'un de l'autre ; `spacetime sql hexmerge "SELECT * FROM user"` montre les positions live.

## Points d'attention / risques

- `BLEND_MULTIPLIED` + render texture sur WebGL/emscripten : à valider tôt (étape 2) — si souci, fallback = dessiner la lightmap en `BLEND_ALPHA` avec une texture d'obscurité inversée (noir troué), même architecture.
- Doublons de couleurs : deux slots peuvent demander la même couleur — le drop remplit le slot visé (pas d'auto-résolution), c'est voulu avec le drag & drop.
- Le joueur peut spawner près de la porte/d'un triangle : le rejection-sampling de l'étape 3 garde une distance minimale au spawn.
