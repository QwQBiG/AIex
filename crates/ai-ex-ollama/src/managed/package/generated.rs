//! Fixed Ollama control files for the verified original Qwen3 4B preset.

pub(super) fn file(path: &str) -> Option<&'static [u8]> {
    Some(match path {
        "config.json" => CONFIG.as_bytes(),
        "params.json" => PARAMETERS.as_bytes(),
        "manifest.json" => MANIFEST.as_bytes(),
        "template" => TEMPLATE.as_bytes(),
        _ => return None,
    })
}

const CONFIG: &str = r#"{"model_format":"gguf","model_family":"qwen3","model_families":["qwen3"],"model_type":"4.0B","file_type":"Q4_K_M","architecture":"amd64","os":"windows","rootfs":{"type":"layers","diff_ids":["sha256:7485fe6f11af29433bc51cab58009521f205840f5b4ae3a32fa7f92e8534fdf5","sha256:0a3e43c71bdec4a59b0ca75916475ff79722c1681b06203ea092bc71e1d0a92f","sha256:5de36594c10839788a8c589443a8ef9d8b8d17c65a1b5807206ae037fc36c6bd","sha256:a52b2de771634a7bbcbc2e46e14958e6dfff5adfe83413389bb94cf4ddcbb02a"]}}"#;
const PARAMETERS: &str = r#"{"stop":["<|im_start|>","<|im_end|>"],"temperature":0.7,"top_p":0.8,"top_k":20,"min_p":0,"presence_penalty":1.5,"repeat_penalty":1.0}"#;
const MANIFEST: &str = r#"{"schemaVersion":2,"mediaType":"application/vnd.docker.distribution.manifest.v2+json","config":{"mediaType":"application/vnd.docker.container.image.v1+json","digest":"sha256:de0d000466e9b88012625eb1c8fe5178eaaf28d187db48eaaa318844340bf827","size":488},"layers":[{"mediaType":"application/vnd.ollama.image.model","digest":"sha256:7485fe6f11af29433bc51cab58009521f205840f5b4ae3a32fa7f92e8534fdf5","size":2497280256},{"mediaType":"application/vnd.ollama.image.template","digest":"sha256:0a3e43c71bdec4a59b0ca75916475ff79722c1681b06203ea092bc71e1d0a92f","size":1517},{"mediaType":"application/vnd.ollama.image.license","digest":"sha256:5de36594c10839788a8c589443a8ef9d8b8d17c65a1b5807206ae037fc36c6bd","size":11544},{"mediaType":"application/vnd.ollama.image.params","digest":"sha256:a52b2de771634a7bbcbc2e46e14958e6dfff5adfe83413389bb94cf4ddcbb02a","size":133}]}"#;
const TEMPLATE: &str = r#"{{- $lastUserIdx := -1 -}}
{{- range $idx, $msg := .Messages -}}
{{- if eq $msg.Role "user" }}{{ $lastUserIdx = $idx }}{{ end -}}
{{- end }}
{{- if or .System .Tools }}<|im_start|>system
{{ if .System }}{{ .System }}

{{ end }}
{{- if .Tools }}# Tools

You may call one or more functions to assist with the user query.

You are provided with function signatures within <tools></tools> XML tags:
<tools>
{{- range .Tools }}
{"type": "function", "function": {{ .Function }}}
{{- end }}
</tools>

For each function call, return a json object with function name and arguments within <tool_call></tool_call> XML tags:
<tool_call>
{"name": <function-name>, "arguments": <args-json-object>}
</tool_call>
{{- end -}}
<|im_end|>
{{ end }}
{{- range $i, $_ := .Messages }}
{{- $last := eq (len (slice $.Messages $i)) 1 -}}
{{- if eq .Role "user" }}<|im_start|>user
{{ .Content }}<|im_end|>
{{ else if eq .Role "assistant" }}<|im_start|>assistant
{{ if (and $.IsThinkSet (and .Thinking (or $last (gt $i $lastUserIdx)))) -}}
<think>{{ .Thinking }}</think>
{{ end -}}
{{ if .Content }}{{ .Content }}{{ end }}
{{- if .ToolCalls }}
{{- range .ToolCalls }}
<tool_call>
{"name": "{{ .Function.Name }}", "arguments": {{ .Function.Arguments }}}
</tool_call>
{{- end }}
{{- end }}{{ if not $last }}<|im_end|>
{{ end }}
{{- else if eq .Role "tool" }}<|im_start|>user
<tool_response>
{{ .Content }}
</tool_response><|im_end|>
{{ end }}
{{- if and (ne .Role "assistant") $last }}<|im_start|>assistant
<think>

</think>

{{ end }}
{{- end }}"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MANAGED_MODEL_FILES;

    #[test]
    fn embedded_control_files_match_installer_digests_exactly() {
        let mut count = 0;
        for item in MANAGED_MODEL_FILES {
            let Some(bytes) = file(item.path) else {
                continue;
            };
            count += 1;
            assert_eq!(bytes.len() as u64, item.bytes);
            let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
            let actual: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(actual, item.sha256, "{}", item.path);
            if item.path.ends_with(".json") {
                serde_json::from_slice::<serde_json::Value>(bytes).unwrap();
            }
        }
        assert_eq!(count, 4);
        assert!(file("model.gguf").is_none());
        assert!(file("LICENSE.txt").is_none());
    }
}
