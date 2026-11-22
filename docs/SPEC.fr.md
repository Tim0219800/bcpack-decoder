# Spécification du flux BCPack (reconstituée)

*Version anglaise (de référence) : [SPEC.md](SPEC.md).*

Ce document décrit le format BCPack de DirectStorage sur Xbox Series, tel
qu'il a été reconstitué à partir des données (voir [Méthode](#9-méthode-salle-blanche-données-seules)).
Tout ce qui est marqué **établi** reproduit de vrais flux à l'octet près ; le
reste est signalé comme hypothèse.

Notation : `x >> n`, `x << n`, `&`, `|` comme en C. Les entiers des en-têtes
d'enregistrement sont petit-boutistes ; les états rANS sont gros-boutistes.

## 1. Vue d'ensemble

BCPack est un codeur entropique sans perte pour les blocs de texture BCn. Un
**chunk** est une suite d'octets autonome qui se décode en un nombre connu
d'octets (`out_size`, fourni par le conteneur). Dans un chunk :

* les champs des blocs sont répartis sur `K` **lanes** (12 pour BC1) ; chaque
  lane porte exactement un symbole par **unité de codage** (8 octets de
  sortie, soit un bloc BC1) ;
* chaque lane est une suite d'**enregistrements** ; chaque enregistrement est
  codé en **rANS** octet par octet, avec un modèle statique choisi par son
  en-tête ;
* les lanes partagent un seul flux d'octets ; un **ordonnanceur**
  déterministe distribue les octets (au plus 8 octets par ronde, par numéro
  de lane).

Il n'y a ni en-tête de chunk, ni répertoire des lanes, ni somme de contrôle :
le premier octet du chunk est le premier octet d'en-tête de la lane 0.

## 2. Enregistrement (établi)

```
mode  len_lo  [len_hi]  état[0 .. min(4, total)]  données ...
```

Octet `mode` :

| bits | sens |
|------|------|
| 0-3  | `q`, indice du modèle (0..15) |
| 4    | état présent ; toujours 1 dans les vraies données (un enregistrement avec le bit 4 à 0 est rejeté) |
| 5    | longueur étendue : `len_hi` suit |
| 6-7  | `r`, réduction de largeur : la largeur des symboles de l'enregistrement est `w' = w - r`, où `w` est la largeur de base de la lane |

Taille `total` de l'enregistrement (octets après le champ de longueur, état
compris) :

* forme courte (bit 5 = 0) : `total = len_lo + 1` (1 à 256) ;
* forme étendue (bit 5 = 1) : `total = (len_lo | len_hi << 8) + 257`
  (257 à 65 792).

Les `nst = min(4, total)` premiers octets sont l'état rANS initial,
**gros-boutiste** : `x` est l'entier formé par ces `nst` octets, poids fort
en premier. Les `total - nst` octets restants sont les données (le **budget**
de la lane).

**Les octets nuls de fin ne sont pas stockés.** L'encodeur supprime les
octets nuls à la fin d'un enregistrement (état compris : un état peut donc
faire 1 à 3 octets). Le décodeur les rétablit implicitement : dès qu'un
enregistrement n'a plus d'octets et que `x < L`, il décale des octets nuls
(`x <<= 8`) jusqu'à `x >= L`, sans coût en rondes.

## 3. Codeur rANS (établi)

* Échelle des probabilités `M = 2^16` (fréquences sur 16 bits, tables de
  somme 65 536).
* Intervalle normalisé de l'état `[L, 256 L)` avec `L = 2^24` ; l'état tient
  sur 32 bits.
* Renormalisation octet par octet : tant que `x < L`,
  `x = (x << 8) | octet_suivant`.
* Décodage d'un symbole avec la table `cum` (fréquences cumulées) :

  ```
  slot = x & 0xFFFF
  trouver s tel que cum[s] <= slot < cum[s+1]
  x    = (cum[s+1] - cum[s]) * (x >> 16) + slot - cum[s]
  ```

* L'encodeur (informatif) est l'encodeur rANS octet par octet standard : il
  part de `x = L`, code les symboles dans l'ordre inverse, émet l'octet de
  poids faible de l'état tant que `x >= 2^16 * freq`, puis stocke l'état final
  en gros-boutiste suivi des octets émis, dans l'ordre inverse d'émission.

### Emplacement FIN et fin d'enregistrement

L'emplacement 0 de chaque table a la fréquence 1 et n'appartient à aucun
symbole réel : c'est l'emplacement **FIN**. Les symboles réels `v = 0..n-1`
occupent les emplacements `cum[0] = 1` à `cum[n] = 65536`. Comme aucun
symbole réel ne possède l'emplacement 0, un état codé n'a jamais
`x & 0xFFFF == 0`, sauf tout à la fin, quand `x` est revenu à l'état initial
de l'encodeur, `L`.

Un enregistrement se termine quand son budget est nul, qu'aucun octet lu
n'attend, et que `x >= L` avec `x == L` ou `slot == 0`. La fin n'est valide
que si `x == L` exactement ; tout autre cas est une erreur. Tomber sur
l'emplacement 0 alors que l'enregistrement a encore des octets est aussi une
erreur.

## 4. Modèle de probabilités (établi)

La table du modèle `q` pour la largeur `w'` (`n = 2^w'` symboles) est
calculée, pas transmise.

### q = 0 : uniforme avec FIN

`cum[0] = 1`, `cum[v] = v * 2^(16-w')` pour `v = 1..n`. Le symbole 0 a donc
`2^(16-w') - 1` emplacements et chacun des autres `2^(16-w')`. Ce détail
compte : un modèle uniforme pur (le symbole 0 possédant l'emplacement 0) ne
décode pas les vrais flux.

### q = 1..15 : loi géométrique tronquée

Avec `rho = exp(-lambda_q / n)` et `F(v) = (1 - rho^v) / (1 - rho^n)` :

1. Paquets : `{0}, {1}, {2,3}, {4..7}, ..., {n/2 .. n-1}` (`w' + 1` paquets).
   Tous les symboles d'un paquet ont la même fréquence.
2. Frontière au début de chaque paquet :
   `B_k = arrondi_pair(65536 * F(début_k))` ; puis `B_0 = 1` (emplacement FIN)
   et `B_dernier = 65536`.
3. Pour les paquets `k = dernier .. 1` (de haut en bas), de `taille_k`
   symboles : `B_k = min(B_k, B_{k+1} - taille_k)` ;
   `f_k = max(1, arrondi_pair((B_{k+1} - B_k) / taille_k))` ;
   `B_k = B_{k+1} - f_k * taille_k`.
4. Le paquet 0 (symbole 0) reçoit `f_0 = B_1 - B_0`.

L'arrondi se fait au pair le plus proche (IEEE `roundTiesToEven`), en double
précision.

| q | lambda_q | q | lambda_q | q | lambda_q |
|---|----------|---|----------|---|----------|
| 1 | 0.7399434 | 6 | 5.6759508 | 11 | 31.4462 |
| 2 | 1.4962494 | 7 | 7.50811 | 12 | 53.67977 |
| 3 | 2.30715745 | 8 | 10.1124375 | 13 | 109.2579 |
| 4 | 3.22101565 | 9 | 14.03737 | 14 | 328.076 |
| 5 | 4.3073742 | 10 | 20.35237 | 15 | 3000 (saturée) |

Les lambdas sont des valeurs ajustées : chacun se trouve dans un intervalle
de valeurs admissibles (tous les enregistrements observés se décodent), et ce
sont les tables entières, pas les lambdas, qui comptent. Exemple,
`(q, w') = (12, 5)` : fréquences par symbole, paquet par paquet,
`53290, 9957, 1104, 14, 1, 1` (FIN = 1 ; total 65 536). `q = 15` est saturée :
tous les symboles sauf 0 ont la fréquence 1.

Le test `model::tests::table_fingerprint` du crate fige une empreinte des
16 × 8 tables : une plateforme dont `exp` ou `pow` différerait serait détectée.

## 5. Ordonnanceur (établi)

Toutes les lanes commencent par lire un en-tête d'enregistrement. Chaque lane
garde :

* `phase` : En-tête, Décodage ou Fini ;
* `buf` : au plus un octet lu pas encore fusionné dans l'état ;
* `budget` : octets de données de l'enregistrement courant pas encore lus ;
* `x` : état rANS ; `pending` : symboles décodés d'avance sur le compteur
  d'unités (0 ou 1) ;
* `fresh` : en-tête terminé dans cette ronde.

Le décodage avance par **rondes**, de quatre étapes chacune.

1. **Demandes.** Une lane *demande* un octet si elle est en phase En-tête, ou
   en phase Décodage avec `buf` vide, `budget > 0`, et (`x < L` ou
   `pending == 0`). En parcourant les lanes par numéro croissant, les **8**
   premières lanes qui demandent un octet sont servies. Une lane en phase
   Décodage qui a demandé un octet sans être servie est **bloquée** pour
   cette ronde.
2. **Lectures.** Les lanes servies, par numéro croissant, prennent chacune
   l'octet suivant du flux. Une lane en En-tête l'ajoute à son en-tête ; quand
   l'en-tête est complet (section 2), elle passe en Décodage avec `fresh`. Une
   lane en Décodage le met dans `buf` et décrémente `budget`.
3. **Décodage.** Pour chaque lane en phase Décodage, par numéro :
   1. si `fresh` : l'effacer, rien d'autre dans cette ronde ;
   2. si `x < L` : si `buf` contient un octet, le fusionner
      (`x = x << 8 | buf`), rien d'autre dans cette ronde ; sinon si
      `budget == 0`, décaler des octets nuls jusqu'à `x >= L` ; sinon
      (en attente d'un octet) rien d'autre ;
   3. si l'enregistrement est fini (section 3) : vérifier `x == L` ; la lane
      passe en Fini si elle a tous ses symboles, sinon elle lit un nouvel
      en-tête ; rien d'autre dans cette ronde ;
   4. si `pending == 1` : rien d'autre ;
   5. si la lane a déjà tous ses symboles : erreur (enregistrement plus long
      que la sortie) ;
   6. si la lane est bloquée : rien d'autre (**règle de la lane non servie** :
      une lane qui a demandé un octet sans l'obtenir ne décode pas dans cette
      ronde, même si son état le permettrait ; les lanes servies décodent
      normalement) ;
   7. décoder un symbole, mettre `pending = 1` ; puis, si `x < L` et que `buf`
      contient un octet, le fusionner tout de suite ; si `budget == 0`, `buf`
      vide et `x < L`, décaler des octets nuls ; enfin appliquer le test de fin
      de l'étape 3.
4. **Synchronisation par unité.** Si chaque lane a `pending == 1` ou est Finie
   avec plus de symboles que le nombre d'unités terminées, remettre `pending`
   à 0 dans toutes les lanes et compter une unité terminée de plus.

Le décodage s'arrête quand toutes les lanes sont Finies. Un chunk valide
vérifie alors : chaque lane a exactement `out_size / 8` symboles, aucune lane
n'a d'octets d'enregistrement non lus, et tous les octets du chunk ont été
consommés. (L'implémentation abandonne aussi après `20 * unités + 200`
rondes.)

Remarques :

* Un en-tête se lit à raison d'un octet par ronde (mode, octet(s) de
  longueur, octets d'état) ; les lanes 8 et suivantes ne commencent donc à
  lire que lorsque moins de 8 lanes de numéro inférieur demandent un octet.
* Fusionner un octet lu coûte une ronde ; rétablir des octets nuls supprimés,
  non.
* Une lane a au plus un symbole d'avance sur la lane la plus lente.

## 6. BC1 (établi)

Lanes (largeurs de base entre parenthèses) :

| lane | largeur | contenu |
|------|---------|---------|
| 0 | 8 | distance de référence arrière, repliée |
| 1 | 8 | toujours 0 dans les données observées (sens inconnu ; le décodeur rejette les autres valeurs) |
| 2, 3, 4 | 5, 6, 5 | deltas zigzag de c0 = (R, G, B) |
| 5, 6, 7 | 5, 6, 5 | deltas zigzag de c1 = (R, G, B) |
| 8, 9, 10, 11 | 8 | quadrants de sélecteurs : haut-gauche, haut-droit, bas-gauche, bas-droit |

Pour le bloc `i` (dans l'ordre du flux) :

* **Distance repliée.** Symbole `v` de la lane 0 : `v = 0` signifie pas de
  référence (prédicteur nul sur tous les canaux) ; `v` pair donne `D = v / 2`
  (1 à 127) ; `v` impair donne `D = 255 - (v >> 1)` (128 à 255). La fenêtre
  fait 255 blocs.
* **Référence.** Le bloc `i - D` si `D <= i` ; sinon l'entrée d'historique
  `-(D - i)`.
* **Historique.** Avant le bloc 0, la fenêtre contient une table statique de
  255 paires d'endpoints (blocs virtuels -1 à -255), identique pour tous les
  chunks. Elle ne contient **pas** les blocs du chunk précédent. Ses valeurs
  sont des constantes apprises et ne font pas partie de cette spécification
  (voir section 8).
* **Endpoints.** Pour chaque canal `c` de `(R0, G0, B0, R1, G1, B1)` :
  `e[i][c] = (e[ref][c] + zigzag(lane[2 + c][i])) mod m_c`, avec
  `m = 32, 64, 32, 32, 64, 32` et `zigzag(v) = v / 2` si `v` est pair,
  `-(v / 2) - 1` s'il est impair (0, -1, 1, -2, 2…). c0 et c1 sont prédits
  indépendamment, canal par canal.
* **Mots de couleur.** `c0 = R0 << 11 | G0 << 5 | B0`, `c1` de même ; écrits
  en petit-boutiste dans les octets 0-1 et 2-3 du bloc.
* **Sélecteurs.** Chaque symbole d'une lane d'index passe par une table
  statique de 256 entrées (une permutation, apprise, voir section 8) qui donne
  un **code de quadrant** : quatre sélecteurs de 2 bits d'un quadrant de
  2 × 2 pixels, bits `2p .. 2p+1` pour le pixel `p` dans l'ordre (0,0), (0,1),
  (1,0), (1,1) (ligne, colonne) du quadrant. Avec les codes `HG, HD, BG, BD` :

  ```
  octet 4 = (HG & 15) | (HD & 15) << 4     ligne 0
  octet 5 = (HG >> 4) | (HD >> 4) << 4     ligne 1
  octet 6 = (BG & 15) | (BD & 15) << 4     ligne 2
  octet 7 = (BG >> 4) | (BD >> 4) << 4     ligne 3
  ```

  Les symboles sont numérotés par fréquence décroissante ; les plus fréquents
  sont les quadrants constants.

**Ordre mémoire et tuilage.** Le bloc `i` du flux correspond aux octets
`8i .. 8i+8` de la sortie du chunk. Un chunk couvre une plage d'octets de la
texture telle qu'elle est rangée en mémoire GPU, c'est-à-dire dans la
disposition tuilée (swizzle AMD Gfx10) ; BCPack ne réordonne pas les blocs.
La fenêtre de référence et l'historique fonctionnent donc dans l'ordre
mémoire, pas dans l'ordre des lignes de l'image, et le détuilage est une
étape distincte, après le décodage. Un chunk peut contenir un niveau de mip
entier, une partie d'un niveau, ou plusieurs petits niveaux.

## 7. État par format

| format | état |
|--------|------|
| BC1 | **résolu** : la couche entropique et la sémantique des blocs reproduisent exactement les vrais chunks |
| BC7 | non résolu. Même format d'enregistrement, mêmes tables, même plafond de 8 lectures ; 16 lanes, largeurs de base `4,4,8,8,8,8,8,7,7,5,6,8,8,8,8,8` (la dernière supposée), un symbole par lane et par 8 octets (deux par bloc BC7). Avec les règles d'ordonnancement de BC1, le décodage se désynchronise après quelques centaines de rondes, typiquement quand une lane redémarre après un court enregistrement `q = 0`. Sémantique non étudiée. |
| BC5 | non résolu. Hypothèse : 12 lanes (`3,4,8,8` puis huit lanes de largeur 6). |
| BC4, BC3, BC6H | non étudiés. |

La couche entropique du crate (`decode_lanes`, commande `lanes --widths`) est
générique sur la disposition des lanes, pour faciliter la suite.

## 8. Fichier de tables

BC1 a besoin de deux tables apprises, chargées à l'exécution depuis un JSON :

```json
{
  "format": "bcpack-tables",
  "version": 1,
  "bc1": {
    "history": [[r0, g0, b0, r1, g1, b1], null, ...],
    "quadrant_codes": [c0, c1, ..., c255]
  }
}
```

* `history` : exactement 255 éléments ; l'élément `k` est l'entrée du bloc
  virtuel `-(k+1)`, ou `null` si elle est inconnue (le décodage n'échoue que
  si un bloc référence réellement une entrée inconnue). R et B vont de 0 à
  31, G de 0 à 63.
* `quadrant_codes` : exactement 256 entiers, une permutation de 0..255 ;
  l'élément `s` est le code de quadrant du symbole d'index `s` (disposition en
  section 6).

La commande `learn-tables` calcule les deux tables à partir de vos propres
données ; voir le README.

## 9. Méthode (salle blanche, données seules)

Le format a été reconstitué uniquement en observant des données : des
statistiques sur plusieurs milliers de chunks BCPack et sur les niveaux de
mip non compressés rangés à côté. Aucun binaire Microsoft, exécutable de jeu,
SDK ni logiciel de console n'a été désassemblé, décompilé ou tracé, et aucune
documentation confidentielle n'a été utilisée. Informations publiques
utilisées : la documentation publique DirectStorage de Microsoft (BCPack est
un codeur entropique rANS pour les données BCn, qui sépare endpoints et
index), des discussions publiques de développeurs de compression (codage sans
perte posé sur des blocs BCn), et des notes publiques de chercheurs
indépendants (rANS octet par octet, probabilités sur 16 bits, `L = 2^24`).

Étapes principales :

1. **Codeur.** Recherche exhaustive parmi les variantes de rANS (taille
   d'état, précision, `L`, ordre des octets, entrelacement). Le critère
   d'acceptation est strict : une bonne configuration décode un
   enregistrement avec un état qui revient exactement à `L` et tous les
   octets consommés, ce que des données aléatoires ne font qu'avec une
   probabilité d'environ `2^-32`.
2. **Enregistrements.** Des statistiques par colonnes sur de nombreux chunks
   ont donné la forme de l'en-tête (mode, longueur, état), confirmée par la
   somme des longueurs égale à la taille des chunks.
3. **Loi de probabilités.** De petits enregistrements complets (décodables
   sans connaître l'ordonnancement) ont fixé les tables entières, paquet par
   paquet ; un seul paramètre par `q` les explique toutes. Les lambdas ont
   ensuite été resserrés jusqu'à ce que tous les enregistrements de tous les
   chunks se décodent. La règle FIN de `q = 0` vient des derniers chunks en
   échec.
4. **Ordonnanceur.** Un simulateur ronde par ronde a été confronté à des
   chunks complets ; chaque règle (8 lectures par ronde, priorité par numéro
   de lane, fusion qui coûte une ronde, octets nuls rétablis gratuitement,
   règle de la lane non servie) n'a été gardée que si elle augmentait le
   nombre de chunks décodés exactement sans en perdre aucun.
5. **Sémantique.** Les lanes décodées ont été comparées au niveau de mip
   suivant stocké sans compression (corrélation après réduction), à des
   contraintes locales exactes (`c0 > c1` dans les blocs opaques à quatre
   couleurs ; sélecteur 3 inutilisé dans les blocs à trois couleurs), et aux
   statistiques de voisinage des quadrants de sélecteurs.
6. **Tables apprises.** Entrées d'historique par moindres carrés contre le
   niveau de mip suivant, puis corrigées par les contraintes exactes ;
   correspondance des quadrants par fréquence, poids moyen et vraisemblance
   des bigrammes contre un corpus de blocs BC1 ordinaires.

Résultat pour BC1 : tous les chunks BC1 du corpus étudié (environ 100 000
chunks) se décodent exactement, et les textures décodées concordent avec les
niveaux de mip inférieurs stockés sans compression (corrélation médiane de
0,998 après réduction).
