#!/usr/bin/env nu

# Fetch the user-defined #133 candidates into the model cache (docs/eval/embed-models.md).
#
# fastembed's hub client is not re-exported, so the two ONNX exports it has no
# built-in entry for — arctic-embed-m-v2.0 and Qwen3-Embedding-0.6B — are
# pulled here, file by file, into `<cache>/candidates/<repo>/`, where
# `agmem_embed::candidates::CandidateBackend::load` reads them. The built-in
# candidates (bge-small, EmbeddingGemma) download through fastembed itself on
# first use and need nothing from this script.
#
#     nu scripts/embed-candidates-fetch.nu
#     nu scripts/embed-candidates-fetch.nu --cache /tmp/agmem-model-cache
#
# Idempotent: a file already present with a non-zero size is skipped. About
# 2.2 GB in total — arctic's fp32 `onnx/model.onnx` (#139, the CoreML
# measurement) is the largest. Set HF_TOKEN for a gated repo.

const REPOS = [
    [repo, files];
    ["Snowflake/snowflake-arctic-embed-m-v2.0" ["onnx/model_int8.onnx" "onnx/model.onnx" "tokenizer.json" "config.json" "special_tokens_map.json" "tokenizer_config.json"]]
    ["onnx-community/Qwen3-Embedding-0.6B-ONNX" ["onnx/model_int8.onnx" "tokenizer.json" "config.json" "special_tokens_map.json" "tokenizer_config.json"]]
]

def main [
    --cache: string = ""    # FASTEMBED_CACHE_DIR; defaults to the platform model cache
] {
    let cache = if ($cache | is-empty) { default-model-cache } else { $cache }
    let headers = if ($env.HF_TOKEN? | is-empty) { [] } else { [Authorization $"Bearer ($env.HF_TOKEN)"] }
    for row in $REPOS {
        let dir = [$cache "candidates" $row.repo] | path join
        for file in $row.files {
            let target = [$dir $file] | path join
            mkdir ($target | path dirname)
            if (($target | path exists) and ((ls $target | get 0.size) > 0b)) {
                print $"skip ($row.repo)/($file) — present, (ls $target | get 0.size)"
                continue
            }
            let url = $"https://huggingface.co/($row.repo)/resolve/main/($file)"
            print $"get  ($row.repo)/($file)"
            http get --raw --headers $headers $url | save -f --raw $target
            print $"     ((ls $target | get 0.size))"
        }
        # An `.onnx_data` sibling, when the export keeps its weights outside
        # the graph; both int8 exports are single files, so a 404 here is the
        # expected answer and `try` swallows it.
        let data = [$dir "onnx/model_int8.onnx_data"] | path join
        if not ($data | path exists) {
            let url = $"https://huggingface.co/($row.repo)/resolve/main/onnx/model_int8.onnx_data"
            let present = (try { http head $url; true } catch { false })
            if $present {
                print $"get  ($row.repo)/onnx/model_int8.onnx_data"
                http get --raw --headers $headers $url | save -f --raw $data
            }
        }
    }
    print $"cache: ($cache)"
}

# The daemon's own model cache, so the candidates sit beside bge-small.
def default-model-cache [] {
    if ($env.FASTEMBED_CACHE_DIR? | is-not-empty) { return $env.FASTEMBED_CACHE_DIR }
    let base = if $nu.os-info.name == "macos" {
        [$nu.home-dir "Library" "Application Support" "dev.agmem.agmem"] | path join
    } else {
        [$nu.home-dir ".local" "share" "agmem"] | path join
    }
    $base | path join "models"
}
