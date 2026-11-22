# bcpack-decoder

A clean-room decoder for **Xbox Series DirectStorage BCPack** texture
streams, written in Rust (library + command-line tool, no dependencies).

BCPack is a lossless entropy coder for BCn texture blocks: the block fields
are split into lanes, each lane is coded with byte-wise rANS under a static
parametric model, and the lanes share one byte stream through a deterministic
read scheduler. This project documents the format and decodes **BC1**.

*Version française plus bas : [Français](#français).*

| format | status |
|--------|--------|
| BC1 | **supported**, bit-exact |
| BC7, BC6H, BC5, BC4, BC3 | entropy layer solved (`lanes --codec`, every chunk of the corpus exact); block reconstruction not implemented |

## Legal notice

* The entropy layer that BCPack uses is described in Microsoft's patent
  **US 11,234,023 B2**, "Features of range asymmetric number system encoding
  and decoding" (filed 2019, granted 2022), with family members including
  **EP 3 991 303 B1** (Europe), WO 2020/263438, CN 114073009 B and
  KR 10-2922298 B1. It covers rANS coding that switches, fragment by fragment,
  between static probability models signalled in a header, with adjustable
  symbol width and selective state flushing. This project is a
  **non-commercial interoperability and research implementation**. If you
  plan any other use, check the patent situation in your jurisdiction first.
* The format was reconstructed from data only (statistics on texture chunks).
  No Microsoft or game binary was disassembled or decompiled, and no
  confidential documentation was used. See [docs/SPEC.md](docs/SPEC.md),
  section "Method".
* No affiliation with, or endorsement by, Microsoft or any game publisher.
  Xbox, DirectStorage and DirectX are trademarks of Microsoft Corporation.
* The repository contains no game data and no learned constant tables (see
  [Tables](#tables)).
* The software is provided "as is", without warranty of any kind (see
  [LICENSE](LICENSE)).

## Build and test

```sh
cargo build --release
cargo test
```

The binary is `target/release/bcpack-decoder`. Rust 1.77 or later.

## Command line

```sh
# Decode one BC1 chunk into N bytes of BC1 blocks (chunk memory order)
bcpack-decoder decode --codec bc1 --tables tables.json --size 32768 chunk.bin blocks.bin

# Entropy layer only: lane symbols, lane after lane (no table file needed)
bcpack-decoder lanes --codec bc1 --size 32768 chunk.bin lanes.bin
bcpack-decoder lanes --widths 4,4,8,8,8,8,8,7,7,5,6,8,8,8,8,8 --size 65536 chunk.bin lanes.bin

# Derive the table file from your own data
bcpack-decoder learn-tables --list samples.txt --ref blocks --out tables.json

# Print one probability table
bcpack-decoder model --q 12 --width 5
```

`--size` is the decoded size of the chunk in bytes (a multiple of 8). It is
not stored in the BCPack stream; it comes from the container that holds the
chunk.

The output is the chunk's bytes as they lie in memory. GPU textures are
usually tiled (swizzled); detiling is a separate step and is not done here.

Every chunk is validated strictly: each record must end exactly on the rANS
FIN condition, every lane must produce exactly `size / 8` symbols and every
input byte must be consumed. Any inconsistency is reported as an error.

## Library

```rust
use bcpack_decoder::{decode_bc1, decode_lanes, Bc1Tables, CodecParams};

let tables = Bc1Tables::load("tables.json".as_ref())?;
let raw = std::fs::read("chunk.bin")?;
let blocks: Vec<u8> = decode_bc1(&raw, 32768, &tables)?;           // BC1 blocks
let lanes = decode_lanes(&raw, &CodecParams::bc1(), 32768)?;       // 12 lanes of symbols
```

`decode_lanes` takes any lane layout (`CodecParams::with_widths`), for work on
the formats that are not solved yet.

## Tables

BC1 needs two **learned** tables:

* the **history**: 255 endpoint pairs that stand for the virtual blocks before
  the start of every chunk (back-references that reach before block 0 read
  them);
* the **quadrant codes**: the mapping from index-lane symbols to 2 x 2
  selector quadrants.

They are constants that were measured, not derived from a formula, and as a
matter of legal caution they are **not shipped** with this project. The
decoder loads them at runtime from a JSON file (format in
[docs/SPEC.md](docs/SPEC.md), section 8), and `learn-tables` derives them from
your own BCPack data. The rANS probability model, which is a parametric law,
is built into the decoder.

### `learn-tables`

The sample list has one line per chunk: `CHUNK SIZE REFERENCE` (tab- or
space-separated; paths relative to the list file; `#` starts a comment).

* `--ref blocks`: `REFERENCE` holds the exact BC1 blocks the chunk decodes to
  (`SIZE` bytes), for example the same texture stored elsewhere without
  BCPack. Both tables follow exactly; the report says how many samples rebuild
  their reference. History entries that no sample uses stay unknown (`null`),
  and only blocks that reference them would fail to decode.
* `--ref quads`: `REFERENCE` holds an approximate colour reference, 12 bytes
  per block: R, G, B of each 2 x 2 quadrant of the block (top-left, top-right,
  bottom-left, bottom-right), in stream order. If a chunk holds exactly one mip
  level and the next level is available uncompressed, each pixel of the next
  level is one quadrant. History entries are fitted by least squares (R and B
  are usually exact, G can be off by one). The quadrant mapping comes from
  `--tables FILE` if given; otherwise from `--corpus LIST`, a list of files of
  plain (non-BCPack) BC1 blocks, by a statistical estimate (frequency, mean
  weight, neighbour statistics) that is reliable for frequent symbols only.

`--tables FILE` also supplies the entries that the samples leave unobserved.
More samples give better tables; exact `blocks` pairs are always preferable.

## Verification

* `cargo test`: model tables (every table sums to 65 536, fingerprint of all
  128 tables), header parsing, rANS round trips for every model, the `q = 0`
  FIN rule, scheduler round trips through a **test-only encoder** (BC1 layout
  and a 16-lane layout, extended-length records), BC1 block rebuild from
  synthetic lanes, table learning on synthetic data, JSON, and error paths
  (truncated, extended and corrupted streams; corruptions never panic).
* Real data (not included): with tables learned from the author's own data,
  this crate decodes 5 000 of 5 000 sampled BC1 chunks of a commercial title
  byte for byte identically to the research decoder, which itself decodes
  every BC1 chunk of that title (about 100 000) exactly.

## Documentation

* [docs/SPEC.md](docs/SPEC.md): bitstream specification, status per format,
  method ([French](docs/SPEC.fr.md)).

## License

MIT, see [LICENSE](LICENSE). The patent notice above still applies: the
licence covers this code, not the BCPack method.

The test-only file `src/verilog_ref.rs` contains tables from Microsoft's
`silicon-rans` sample (MIT); see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

---

## Français

Un décodeur « salle blanche » des flux de textures **BCPack de DirectStorage
sur Xbox Series**, écrit en Rust (bibliothèque et outil en ligne de
commande, sans dépendance).

BCPack est un codeur entropique sans perte pour les blocs de texture BCn :
les champs des blocs sont répartis en lanes, chaque lane est codée en rANS
octet par octet avec un modèle paramétrique statique, et les lanes partagent
un seul flux d'octets grâce à un ordonnanceur de lecture déterministe. Ce
projet documente le format et décode le **BC1**.

| format | état |
|--------|------|
| BC1 | **pris en charge**, exact au bit près |
| BC7, BC6H, BC5, BC4, BC3 | couche entropique résolue (`lanes --codec`, tous les chunks du corpus exacts) ; reconstruction des blocs non implémentée |

### Mentions légales

* La couche entropique utilisée par BCPack est décrite dans le brevet
  Microsoft **US 11,234,023 B2**, « Features of range asymmetric number system
  encoding and decoding » (déposé en 2019, délivré en 2022), dont la famille
  comprend notamment **EP 3 991 303 B1** (Europe), WO 2020/263438,
  CN 114073009 B et KR 10-2922298 B1. Il couvre un codage rANS qui change,
  fragment par fragment, de modèle de probabilités statique signalé dans un
  en-tête, avec une largeur de symbole réglable et une remise à zéro sélective
  de l'état. Ce projet est une **implémentation non commerciale,
  d'interopérabilité et de recherche**. Pour tout autre usage, vérifiez
  d'abord la situation des brevets dans votre pays.
* Le format a été reconstitué uniquement à partir des données (statistiques
  sur des chunks de textures). Aucun binaire Microsoft ou de jeu n'a été
  désassemblé ou décompilé, et aucune documentation confidentielle n'a été
  utilisée. Voir [docs/SPEC.fr.md](docs/SPEC.fr.md), section « Méthode ».
* Aucun lien avec Microsoft ni avec un éditeur de jeux, et aucune approbation
  de leur part. Xbox, DirectStorage et DirectX sont des marques de Microsoft
  Corporation.
* Le dépôt ne contient aucune donnée de jeu ni aucune table de constantes
  apprises (voir [Tables](#tables-1)).
* Le logiciel est fourni « tel quel », sans aucune garantie (voir
  [LICENSE](LICENSE)).

### Compiler et tester

```sh
cargo build --release
cargo test
```

Le binaire est `target/release/bcpack-decoder`. Rust 1.77 ou plus récent.

### Ligne de commande

```sh
# Décoder un chunk BC1 en N octets de blocs BC1 (ordre mémoire du chunk)
bcpack-decoder decode --codec bc1 --tables tables.json --size 32768 chunk.bin blocks.bin

# Couche entropique seule : symboles des lanes, lane après lane (sans fichier de tables)
bcpack-decoder lanes --codec bc1 --size 32768 chunk.bin lanes.bin
bcpack-decoder lanes --widths 4,4,8,8,8,8,8,7,7,5,6,8,8,8,8,8 --size 65536 chunk.bin lanes.bin

# Calculer le fichier de tables à partir de vos propres données
bcpack-decoder learn-tables --list samples.txt --ref blocks --out tables.json

# Afficher une table de probabilités
bcpack-decoder model --q 12 --width 5
```

`--size` est la taille décodée du chunk en octets (un multiple de 8). Elle
n'est pas stockée dans le flux BCPack : elle vient du conteneur qui porte le
chunk.

La sortie est formée des octets du chunk tels qu'ils sont rangés en mémoire.
Les textures GPU sont en général tuilées (swizzle) ; le détuilage est une
étape distincte, non faite ici.

Chaque chunk est validé strictement : chaque enregistrement doit se terminer
exactement sur la condition FIN du rANS, chaque lane doit produire exactement
`size / 8` symboles et tous les octets d'entrée doivent être consommés. Toute
incohérence est signalée par une erreur.

### Bibliothèque

```rust
use bcpack_decoder::{decode_bc1, decode_lanes, Bc1Tables, CodecParams};

let tables = Bc1Tables::load("tables.json".as_ref())?;
let raw = std::fs::read("chunk.bin")?;
let blocks: Vec<u8> = decode_bc1(&raw, 32768, &tables)?;           // blocs BC1
let lanes = decode_lanes(&raw, &CodecParams::bc1(), 32768)?;       // 12 lanes de symboles
```

`decode_lanes` accepte n'importe quelle disposition de lanes
(`CodecParams::with_widths`), pour travailler sur les formats pas encore
résolus.

### Tables

BC1 a besoin de deux tables **apprises** :

* l'**historique** : 255 paires d'endpoints qui tiennent lieu des blocs
  virtuels situés avant le début de chaque chunk (les références arrière qui
  remontent avant le bloc 0 les lisent) ;
* les **codes de quadrant** : la correspondance entre les symboles des lanes
  d'index et les quadrants de sélecteurs 2 × 2.

Ce sont des constantes mesurées, pas tirées d'une formule ; par prudence
juridique, elles **ne sont pas fournies** avec ce projet. Le décodeur les
charge à l'exécution depuis un fichier JSON (format dans
[docs/SPEC.fr.md](docs/SPEC.fr.md), section 8), et `learn-tables` les calcule
à partir de vos propres données BCPack. Le modèle de probabilités rANS, qui
est une loi paramétrique, est intégré au décodeur.

#### `learn-tables`

La liste d'échantillons a une ligne par chunk : `CHUNK TAILLE RÉFÉRENCE`
(séparés par des tabulations ou des espaces ; chemins relatifs au fichier de
liste ; `#` commence un commentaire).

* `--ref blocks` : `RÉFÉRENCE` contient les blocs BC1 exacts que donne le
  chunk (`TAILLE` octets), par exemple la même texture stockée ailleurs sans
  BCPack. Les deux tables s'en déduisent exactement ; le rapport indique
  combien d'échantillons redonnent leur référence. Les entrées d'historique
  qu'aucun échantillon n'utilise restent inconnues (`null`) ; seuls les blocs
  qui les référencent échoueraient au décodage.
* `--ref quads` : `RÉFÉRENCE` contient une référence de couleur approchée,
  12 octets par bloc : R, V, B de chaque quadrant 2 × 2 du bloc (haut-gauche,
  haut-droit, bas-gauche, bas-droit), dans l'ordre du flux. Si un chunk
  contient exactement un niveau de mip et que le niveau suivant est disponible
  sans compression, chaque pixel du niveau suivant est un quadrant. Les
  entrées d'historique sont ajustées par moindres carrés (R et B en général
  exacts, G parfois à une unité près). La correspondance des quadrants vient
  de `--tables FICHIER` s'il est donné ; sinon de `--corpus LISTE`, une liste
  de fichiers de blocs BC1 ordinaires (sans BCPack), par une estimation
  statistique (fréquence, poids moyen, statistiques de voisinage) qui n'est
  fiable que pour les symboles fréquents.

`--tables FICHIER` fournit aussi les entrées que les échantillons ne
couvrent pas. Plus d'échantillons donnent de meilleures tables ; des paires
exactes `blocks` sont toujours préférables.

### Vérification

* `cargo test` : tables du modèle (chaque table a une somme de 65 536,
  empreinte des 128 tables), lecture des en-têtes, allers-retours rANS pour
  chaque modèle, règle FIN de `q = 0`, allers-retours de l'ordonnanceur avec
  un **encodeur réservé aux tests** (disposition BC1 et disposition à
  16 lanes, enregistrements à longueur étendue), reconstruction de blocs BC1 à
  partir de lanes synthétiques, apprentissage des tables sur données
  synthétiques, JSON, et cas d'erreur (flux tronqués, allongés et corrompus ;
  une corruption ne provoque jamais de panique).
* Données réelles (non fournies) : avec des tables apprises sur les données
  de l'auteur, ce crate décode 5 000 chunks BC1 sur 5 000, tirés d'un titre
  commercial, à l'octet près comme le décodeur de recherche, qui décode
  lui-même exactement tous les chunks BC1 de ce titre (environ 100 000).

### Documentation

* [docs/SPEC.fr.md](docs/SPEC.fr.md) : spécification du flux, état par
  format, méthode ([anglais](docs/SPEC.md)).

### Licence

MIT, voir [LICENSE](LICENSE). La mention de brevet ci-dessus reste valable :
la licence couvre ce code, pas la méthode BCPack.

Le fichier de test `src/verilog_ref.rs` contient des tables de l'exemple
`silicon-rans` de Microsoft (MIT) ; voir [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
