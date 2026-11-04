# The embedding models the image ships for `zg`, laid out exactly where its own
# downloader would have put them, so a fresh guest can build an index offline on
# its first run: with no completion marker `zg` validates a cached file by size and
# SHA-256 and never touches the network when it matches, and the marker write it
# attempts afterwards is best-effort (the store is read-only).
#
# Two models, because cang picks by hardware (see guest-init's `zg` component):
#   - `local/potion-code-16m-v2`, the CPU default: a 32 MB Model2Vec model that
#     needs no accelerator and indexes a small workspace in well under a second;
#   - `local/qwen3-embedding-0.6b`, the GPU default: llama.cpp GGUF, 610 MiB, and
#     the reason the image's `zg` carries a Vulkan backend at all.
#
# Every path, revision and digest below is the pinned revision's own catalog entry
# (`rust/crates/zg-engine/src/models/catalog/entries.rs`) - the contract `zg`
# re-checks before it uses a cached file. A `pins.zvecGrep` bump that moves a
# revision or a digest has to move these with it, or the guest re-downloads.
{ pkgs }:
let
  # `local/potion-code-16M-v2`: files live under
  # `models/model2vec/<repo with / -> -->/<revision>/`, the model at the top and
  # the tokenizer in `tokenizer/tokenizer.json`.
  potionRevision = "e9d2a44ca6a05ac6685f3b23709ea57eb7352d5b";
  potionRepo = "minishlab/potion-code-16M-v2";
  potionFile = name: hash: {
    url = "https://huggingface.co/${potionRepo}/resolve/${potionRevision}/${name}";
    inherit hash name;
  };
  # `local/qwen3-embedding-0.6b`: a llama.cpp model is keyed by its own
  # `cache_file` at the cache root, which upstream derives as
  # `hf_<repo with / -> _>_<file>`, not by repo and revision.
  qwen3File = {
    url = "https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF/resolve/370f27d7550e0def9b39c1f16d3fbaa13aa67728/Qwen3-Embedding-0.6B-Q8_0.gguf";
    hash = "sha256-BlB8e0JohGnE5ymLCh4W3v8GyvKRzwpbJ4wwgknD5Dk=";
    name = "hf_Qwen_Qwen3-Embedding-0.6B-Q8_0.gguf";
  };
  fetch = spec: pkgs.fetchurl { inherit (spec) url hash name; };
  potionModel = potionFile "model.safetensors" "sha256-dc96bCFxsjCtGbHn2OCxruhtpaAq+OfKzt2ZIdInYjw=";
  potionTokenizer = potionFile "tokenizer.json" "sha256-EHu9y61L/x0pm3pMOi+xfFKJBoi33Q5Mneq3nTxPPUU=";
in
pkgs.runCommand "cang-zg-models" { } ''
  potion=$out/models/model2vec/minishlab--potion-code-16M-v2/${potionRevision}
  install -Dm444 ${fetch potionModel} $potion/model.safetensors
  install -Dm444 ${fetch potionTokenizer} $potion/tokenizer/tokenizer.json
  install -Dm444 ${fetch qwen3File} $out/models/${qwen3File.name}
''
