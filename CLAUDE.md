# Xeneon Dashboard

App GTK4/Libadwaita (Python, PyGObject) affichant un dashboard sur l'écran
barre Corsair Xeneon Edge. Lancée via `python -m xeneon_dashboard`.

## Widgets (plugins) — tailles et disposition

Le dashboard est découpé en une grille de widgets/plugins déplaçables mais
**non redimensionnables librement** : chaque widget choisit une taille
fixe parmi trois presets, à la iCUE. Voir
[xeneon_dashboard/grid.py](xeneon_dashboard/grid.py).

- `SIZE_S`, `SIZE_M`, `SIZE_L` — largeur identique pour les trois (les
  widgets s'empilent en colonnes simples), hauteur en progression S = ⅙
  de L, M = ½ de L (2×M ou 6×S empilés avec `GAP` entre eux = la hauteur
  d'un widget L, au pixel près). Tailles calées sur la résolution physique
  du Xeneon Edge, **2560×720** — l'échelle d'affichage de cet écran est
  fixée à 100% dans les réglages GNOME (Réglages > Écrans), donc l'espace
  logique que GTK utilise pour positionner fenêtres et widgets correspond
  exactement aux pixels physiques. **Si quelqu'un remet une échelle
  fractionnaire sur cet écran (125%, 133%…), ces tailles ne
  correspondront plus** à l'espace réellement disponible et devront être
  recalculées sur la nouvelle résolution logique (`Gdk.Monitor.get_geometry()`
  donne la valeur exacte). 3 colonnes de 832px tiennent exactement dans la
  largeur (`3×832 + 4×GAP = 2560`), et `SIZE_L = (832, 688)` remplit
  exactement la hauteur (`720 - 2×GAP`).
- `SIZE_SQ` (« carré ») — M coupé en deux verticalement, **avec un `GAP`
  entre les deux moitiés** comme partout ailleurs dans la grille (pas de
  widgets collés bord à bord). Même hauteur que M. Deux `SIZE_SQ` côte à
  côte + `GAP` occupent le même espace qu'un `SIZE_M`. Sur l'écran entier
  ça donne 6 `SIZE_SQ` en largeur × 2 en hauteur.
- `SIZE_SX` — même découpage que `SIZE_SQ` mais appliqué à S plutôt qu'à
  M : S coupé en deux verticalement, `GAP` entre les deux moitiés, même
  hauteur que S. Deux `SIZE_SX` côte à côte + `GAP` occupent le même
  espace qu'un `SIZE_S`.
- `SIZE_SSX` — même principe encore une fois, appliqué à SX : SX coupé en
  deux verticalement, `GAP` entre les deux moitiés, même hauteur que SX
  (et donc que S). Deux `SIZE_SSX` côte à côte + `GAP` occupent le même
  espace qu'un `SIZE_SX`.
- `GAP` — écart entre deux widgets, **volontairement identique** à
  `PAGE_MARGIN` (la marge de bord d'écran définie dans
  [window.py](xeneon_dashboard/window.py)) pour que la mise en page ait
  un rythme visuel cohérent. Toute nouvelle disposition doit réutiliser
  `GAP`/`PAGE_MARGIN`, jamais une valeur d'espacement en dur.

**Quand on crée un nouveau plugin/widget** : demander à l'utilisateur
quelle taille (S, M, L ou carré) lui donner, plutôt que de deviner une
taille en pixels. Le widget reçoit alors `size=grid.SIZE_S` (ou
`SIZE_M`/`SIZE_L`/`SIZE_SQ`) tel quel — voir la signature de
`DashboardWidget.__init__` dans grid.py.

Chaque `DashboardWidget` affiche, au survol, un bouton de configuration
(icône engrenage) qui ouvre un popover d'apparence commun à tous les
widgets — transparence, couleur/image de fond, bordure, coins arrondis,
voir [xeneon_dashboard/widget_appearance.py](xeneon_dashboard/widget_appearance.py).
Un plugin peut passer son propre widget de réglages (`settings=...` au
constructeur de `DashboardWidget`) ; il s'affiche à côté des réglages
d'apparence dans ce même popover — voir `ClockSettings` dans
[xeneon_dashboard/widgets/clock.py](xeneon_dashboard/widgets/clock.py)
comme exemple de référence.

## Persistance de la configuration

Rien ne doit se perdre à la fermeture de l'app : la config générale et la
disposition des widgets sont rechargées telles quelles au lancement
suivant. Deux fichiers séparés, tous deux sous
`$XDG_CONFIG_HOME/xeneon-dashboard/` (typiquement `~/.config/xeneon-dashboard/`) :

- **Config de l'app** — un seul fichier `config.json`
  ([xeneon_dashboard/config.py](xeneon_dashboard/config.py)) : langue,
  raccourci plein écran, délai de l'indicateur de page. `config.load()`/
  `config.save()` fusionnent avec `DEFAULTS`, donc ajouter un réglage
  global ne casse rien pour une config existante qui ne le connaît pas
  encore. Voir `XeneonApp` dans [app.py](xeneon_dashboard/app.py) pour le
  patron à suivre (`self.config[...] = valeur; config.save(self.config)`).
- **Config par widget** — un fichier JSON par instance de widget, sous
  `widgets/<id>.json` ([xeneon_dashboard/widget_store.py](xeneon_dashboard/widget_store.py)),
  `<id>` étant un UUID généré à la création (`DashboardWidget.widget_id`).
  Chaque fichier contient `kind`, `page_index`, `x`/`y`/`w`/`h`, `appearance`
  (sortie de `WidgetAppearance.to_dict()`) et, si le plugin en a, `content`
  (sortie de `<ContentClass>.to_dict()`). Au lancement, `XeneonWindow`
  reconstruit toute la disposition à partir de ces fichiers
  (`widget_store.load_all()` + `widget_picker.build_from_state()`) ; s'il
  n'y en a aucun (premier lancement), elle retombe sur la disposition de
  démo codée en dur.

**Quand on crée un nouveau plugin avec des réglages propres** (comme
`ClockSettings`) :
1. Choisir une chaîne `kind` unique (ex. `"thermo"`) et l'enregistrer dans
   `CATALOG` et `build_from_state()` de
   [xeneon_dashboard/widget_picker.py](xeneon_dashboard/widget_picker.py),
   à côté des entrées `clock`/`dummy_*` existantes.
2. Donner à la classe de contenu du plugin deux méthodes, sur le modèle de
   `ClockContent.to_dict()`/`apply_dict()` :
   - `to_dict(self) -> dict` — les réglages du plugin (pas l'apparence
     générique, déjà couverte par `WidgetAppearance`), en types simples
     JSON-sérialisables ;
   - `apply_dict(self, data: dict) -> None` — ne touche que les clés
     présentes dans `data` (compatible avec un fichier plus ancien/partiel),
     et rappelle les mêmes setters que l'UI utiliserait.

   Ces deux méthodes suffisent : `XeneonWindow._save_widget()` détecte
   `to_dict` par duck-typing (`getattr(widget.content, "to_dict", None)`),
   pas besoin de toucher window.py. Si le plugin n'a pas de réglages
   propres (comme les widgets de démo), ne rien ajouter.
3. Si le plugin a un widget de réglages type `ClockSettings`, construire
   son état initial (position des switches, sélection des dropdowns...)
   à partir de l'état déjà restauré sur `content`, pas de valeurs par
   défaut codées en dur — sinon le popover réaffiche l'ancien réglage
   après un redémarrage tant qu'on n'y a pas retouché.

La sauvegarde d'un widget se déclenche à la fermeture de son popover de
configuration (un seul point d'accroche pour apparence + réglages du
plugin, voir le signal `"closed"` branché dans
`DashboardWidget.__init__`) et à la fin d'un déplacement ; sa suppression
efface aussi son fichier. Pas besoin d'appeler quoi que ce soit
manuellement pour persister un changement fait via ces chemins-là.

## Internationalisation (i18n) — obligatoire pour tout nouveau texte

L'app est multilingue (français/anglais pour l'instant). **Toute chaîne de
texte visible par l'utilisateur doit passer par le système de traduction**,
même pour un ajout mineur ou un plugin.

### Convention à suivre pour tout nouveau code

1. Ne jamais écrire de texte en dur dans un widget :
   ```python
   # Interdit
   Gtk.Label(label="Température")

   # Correct
   from xeneon_dashboard import i18n
   Gtk.Label(label=i18n._("widgets.thermo.title"))
   ```
2. Ajouter la clé et sa valeur dans **les deux** fichiers :
   - [xeneon_dashboard/locales/fr.json](xeneon_dashboard/locales/fr.json)
   - [xeneon_dashboard/locales/en.json](xeneon_dashboard/locales/en.json)

   Convention de nommage des clés : `<domaine>.<élément>.<partie>`, ex.
   `settings.language_row.title`, `widgets.clock.placeholder`.
3. Si le widget doit refléter un changement de langue en direct (sans
   redémarrage — c'est le comportement actuel partout dans l'app) :
   - garder une référence à chaque label/widget texte (`self._title = ...`)
     plutôt qu'une variable locale perdue après construction ;
   - implémenter/étendre une méthode `_retranslate(self)` qui réapplique
     `i18n._(...)` sur chacune de ces références ;
   - enregistrer cette méthode avec `i18n.on_change(self._retranslate)` à
     la fin du `__init__`.

   Voir [xeneon_dashboard/settings_page.py](xeneon_dashboard/settings_page.py)
   et [xeneon_dashboard/window.py](xeneon_dashboard/window.py) comme
   exemples de référence.

### Comment ça marche (xeneon_dashboard/i18n.py)

- Chaque langue = un fichier JSON plat `locales/<code>.json`
  (`{"clé": "texte", ...}`), avec une clé spéciale `_language_name` qui
  nomme la langue dans sa propre langue (affichée dans le sélecteur).
- Les langues disponibles dans les réglages sont **découvertes
  automatiquement** depuis les fichiers présents dans `locales/` — ajouter
  une langue ne nécessite aucune modification de code Python, juste un
  nouveau fichier JSON avec les mêmes clés que `fr.json`.
- `i18n._("ma.cle")` retombe sur la version française si la clé manque
  dans la langue active, et sur la clé brute si elle manque partout (utile
  pour repérer visuellement un oubli de traduction).

### Vérifier que rien n'a été oublié

Pour un audit rapide sans relire tout le code : comparer les clés
utilisées dans les `.py` à celles présentes dans `fr.json`. (Le motif
cherche toute chaîne à points en minuscules, pas seulement les appels
`i18n._("...")` littéraux, pour aussi capter les clés référencées via une
variable, comme dans `window.py`.)

```bash
grep -rhoE '"[a-z_]+(\.[a-z_]+){1,}"' xeneon_dashboard --include='*.py' | tr -d '"' | sort -u > /tmp/used_keys.txt
python3 -c "import json; print('\n'.join(sorted(json.load(open('xeneon_dashboard/locales/fr.json')).keys())))" | grep -v '^_' > /tmp/defined_keys.txt
echo "clés définies mais jamais utilisées :"; comm -13 /tmp/used_keys.txt /tmp/defined_keys.txt
echo "clés utilisées mais absentes de fr.json :"; comm -23 /tmp/used_keys.txt /tmp/defined_keys.txt
```

(La deuxième liste peut inclure quelques faux positifs sans rapport avec
l'i18n, ex. un nom de fichier comme `config.json` — à vérifier au cas par
cas, mais ça reste un point de départ bien plus rapide qu'une relecture
complète.)

Repérer aussi les chaînes en dur potentiellement oubliées (texte contenant
un accent ou un mot français, hors fichiers de locale) :

```bash
grep -rnE '"[^"]*[éèêàôùç][^"]*"' xeneon_dashboard --include='*.py'
```

Ces deux commandes suffisent pour localiser précisément ce qui manque
avant une session de traduction, sans avoir à inspecter tout le projet.
