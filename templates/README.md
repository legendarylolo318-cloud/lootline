# Sound templates

`index.json` lists the template files the app downloads (`"templates": ["chest.json"]`).
Each template is a trimmed log-mel spectrogram of the chest-open sound (see `core/src/dsp.rs`).

None are shipped yet: record one in the app (Settings -> Sound templates -> Record template),
copy the resulting `templates/<name>.json` from your data folder here, and list it in `index.json`.
Re-record after a season changes the sound.
