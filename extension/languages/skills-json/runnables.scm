; Runnables for `skills.json` (schema v2 manifest).
;
; ▶ on each by-package `remote[]` entry: matches tasks tagged
; `skills-sync-vendor` and exposes the package as $ZED_CUSTOM_SKILLS_PACKAGE
; (per-vendor sync: `skills update <vendor/package>`).
;
; By-url entries (`"from": "http" | "zip"` with a "url" key and no "package")
; intentionally get no per-entry runnable: the CLI positional filter only
; accepts `vendor/package` / `vendor/*` patterns, not URLs.
;
; The #match? guard mirrors the CLI's VendorPattern (exactly one `/`):
; GitLab subgroup packages (`group/sub/project`) are excluded too, because
; `skills update group/sub/project` would be rejected — drop the guard once
; the positional filter learns multi-slash package names.
((document
  (object
    (pair
      key: (string
        (string_content) @_remote
        (#eq? @_remote "remote"))
      value: (array
        (object
          (pair
            key: (string
              (string_content) @_package
              (#eq? @_package "package"))
            value: (string
              (string_content) @run @SKILLS_PACKAGE
              (#match? @SKILLS_PACKAGE "^[^/]+/[^/]+$"))))))))
  (#set! tag skills-sync-vendor))

; ▶ on the root-level "target" field: matches whole-manifest tasks tagged
; `skills-sync` (`skills update`, `skills update --dry-run`). Anchoring on
; "target" (not the root object) keeps `skills.lock` — same language, no
; root "target" key — free of a misleading file-level runnable.
((document
  (object
    (pair
      key: (string
        (string_content) @_target
        (#eq? @_target "target"))
      value: (string) @run)))
  (#set! tag skills-sync))
