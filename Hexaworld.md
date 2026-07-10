# Hexaworld (Community pixel map)

## Player

1 joueur = 1 couleur aleatoire de départ
chaque joueur a une palette qui lui est propre
il peut reset son compte pour avoir une nouvelle couleur de départ

max 20 tiles/20sec (anti spam)
On devrait voir son propre curseur avec sa couleur (pos en local, pas en réseau par contre)

Un cookie sauvegarde l'identifiant unique du joueur pour qu'il retrouve sa couleur et son inventaire à chaque connexion
Cela permet de ne pas augmenter le nombre total de joueur à chaque rafraichissement de la page
On doit pouvoir se connecter en tant que joueur avec l'identifiant unique (comme sur cookie clicker ou battlecats)

### Inventory

### Merge
quand nouvelle couleur: les 2 joueurs qui l'ont debloquées sont crédités
Deux curseur qui se rencontre merge la couleur
La couleur est sauvegardée dans notre inventaire et on garde la trace de QUAND et AVEC QUI on l'a obtenue

### Admin
1 identifiant avec un mot de passe qui est admin
peut supprimer les tiles
peut freeze la partie pour que plus personne n'intéragisse
Peut restaurer des backups
Détient la tile centrale

## Map
De multiples ilots qui se placent côte à côte (avec une marge d'environ 2 tile dessinable)
L'îlot original est celui de l'Admin
Plus un îlot est liké plus il est proche du centre (une sorte de leaderboard caché)

### Tiles
info couleur (laquelle + celui qui a débloqué la couleur)
info du joueur qui l'a placé
lien optionnel

Outil lien qui permet de mettre ton unique lien sur la tile que tu veux (une des tiennes)

### Ilot
Taille de 14 tile de côté d'hexagone
Les Ilots ne sont modifiables que par les joueurs

Cliquer sur l'ilot de quelqu'un d'autre permet de voirdes informations (créateur, lien itch.io de son jeu, date de création, nombre de like, etc)
Chaque nouveau joueur crée un nouvel îlot
La couleur par défaut des tiles sur un ilot est le blanc

## Color Picker

Hue obtenable en mergeant
Saturation = LVL
Luminosité changeable

## Obtenir de l'experience
Merger avec quelqu'un
Cliquer sur les liens des autres
Temps passé
Cadeau volant qui te donne une couleur ou de l'xp
Like donne XP

## UX

### Déplacement
Sur PC, scroller pour zoom, ou SHIT+CLICK pour se déplacer
Sur mobile, deux doigts pour zoomer et se déplacer

### Header
En grisé et transparent, son identifiant unique et son niveau
Nombre de joueurs actuellement connecté/ nombre total de joueurs

### Footer

Bouton pour centrer sur son ilot
3 hexagons des 3 dernières couleurs utilisées
Bouton pour ouvrir l'inventaire
Champ de texte pour son pseudo

Bouton Lock pour pas être merged automatiquement avec les autres

