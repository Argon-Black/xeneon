# Contribuer à Xeneon Dashboard

Merci de l'intérêt porté à ce projet ! Quelques repères avant d'ouvrir une PR.

## Mettre en place l'environnement

Voir la section [Compilation](README.md#compilation) du README pour les
dépendances système, puis :

    cd xeneon-dashboard-rs
    cargo build
    cargo test -p xeneon-core

## Style de code

- `xeneon-core` reste sans dépendance GTK : toute logique testable sans
  affichage (grille, i18n, persistance...) y va, pas dans `xeneon-app`.
- Dépendances minimales : avant d'ajouter une crate, vérifiez qu'une
  interface système déjà disponible (D-Bus via `gio`, sysfs, etc.) ne
  suffit pas.
- Commentaires : ce projet documente le *pourquoi* plus que la moyenne
  (contraintes, comportements contre-intuitifs, historique d'un bug) —
  gardez ce niveau de détail dans le code que vous ajoutez ou modifiez.

## Messages de commit

Format `<module>: <description au présent, minuscule>`, par ex.
`xeneon-app: fix widget picker close button pushed off-screen`.
Un commit = un changement cohérent et testé.

## Pull requests

- Vérifiez que `cargo build` et `cargo test -p xeneon-core` passent.
- Décrivez le comportement avant/après si le changement touche l'UI
  (une capture d'écran aide beaucoup).
