# Changelog

## [0.3.1](https://github.com/roxblnfk/zed-skills/compare/v0.3.0...v0.3.1) (2026-07-07)


### Features

* **audit:** share the agentskills name rules and warn on bad skill dir names ([2aec356](https://github.com/roxblnfk/zed-skills/commit/2aec356eea4a7060d95bdb6fa7aed0eece965448))
* **core:** abort on FS-dangerous skill dir names, normalize the conflict key ([74102d5](https://github.com/roxblnfk/zed-skills/commit/74102d5d929f42a732426a1d0b9f1c9c3f2081e4))
* **lsp:** document links in skills.json ([c6ef5c6](https://github.com/roxblnfk/zed-skills/commit/c6ef5c663674e80ee8ab816672d761cc7187773e))
* **lsp:** hover docs on SKILL.md frontmatter keys ([d687514](https://github.com/roxblnfk/zed-skills/commit/d687514642dc91f3a6b16aad54e0b5d3445eee15))

## [0.3.0](https://github.com/roxblnfk/zed-skills/compare/v0.2.0...v0.3.0) (2026-07-07)


### Features

* **extension:** yaml frontmatter and embedded-language injections for SKILL.md ([45333c1](https://github.com/roxblnfk/zed-skills/commit/45333c19b1d147858621f9b1c7e221853f7820f8))
* **lsp:** frontmatter validation diagnostics for SKILL.md ([9b30905](https://github.com/roxblnfk/zed-skills/commit/9b309054004b8c45243c10753b250bc56f5dfdca))
* **lsp:** generate and maintain .zed/tasks.json for Zed gutter runnables ([d58379d](https://github.com/roxblnfk/zed-skills/commit/d58379d448aa1c0be699a8bee759690008aec851))
* **lsp:** honor the lock-file manifest option in analysis and watch set ([b23fce4](https://github.com/roxblnfk/zed-skills/commit/b23fce4d24311bf8f0e547784cc14ea3fa8d3dbe))


### Bug Fixes

* **cli:** point generated manifests at the real published JSON Schema ([912da37](https://github.com/roxblnfk/zed-skills/commit/912da3713eac2f2c64c1b35f671b3a756c2d72f9))

## [0.2.0](https://github.com/roxblnfk/zed-skills/compare/v0.1.0...v0.2.0) (2026-07-06)


### Features

* **cli:** skills update --check with exit code 5 (changes pending) ([700bf51](https://github.com/roxblnfk/zed-skills/commit/700bf51fd52915e3fcc624aa375126ccc71f7d68))
* **core:** configurable lockfile location via `lock-file` manifest option ([de05682](https://github.com/roxblnfk/zed-skills/commit/de05682438a321124dc4d2b68a788594690daacf))
* **extension:** bundled skills: check task; create_worktree hook docs ([870c895](https://github.com/roxblnfk/zed-skills/commit/870c895959f04ebb80ee4667001a7e88deaffa68))
* **extension:** gutter runnables + bundled tasks for skills.json sync ([4a5a317](https://github.com/roxblnfk/zed-skills/commit/4a5a31791453b7ae5776e84e3cd4d105eed4ded5))
* **lsp:** SKILL.md frontmatter completion ([bf6ed34](https://github.com/roxblnfk/zed-skills/commit/bf6ed34a76e51ffe83510eb0ae4a6e2c28febf1a))
* **zed:** extension crate — languages, grammars, skills lsp wiring ([b792479](https://github.com/roxblnfk/zed-skills/commit/b792479527b380ab5a5ddc90c6ea138f9ebd4ac5))

## 0.1.0 (2026-07-06)


### Features

* **audit:** StaticAuditor with dangerous-pattern table, llm/http stubs, chain builder ([1f5b146](https://github.com/roxblnfk/zed-skills/commit/1f5b146c12dc521902b5bf5836d75482abbba156))
* **cli:** --re-audit flag, config-driven audit chain, [audit] output in update and show ([1555702](https://github.com/roxblnfk/zed-skills/commit/15557029507ab5780ed36e70b3a0d411a956c6ab))
* **cli:** positional package filters, --trust, --discovery; show annotations ([a2834f5](https://github.com/roxblnfk/zed-skills/commit/a2834f519240c0a0c922b2cbcae7e38a0d903ede))
* **cli:** remote provider wiring, --from/--refresh, skills add, cache gitignore ([90f8a0e](https://github.com/roxblnfk/zed-skills/commit/90f8a0e190783f08bbc11ecc8f65d620adcea20a))
* **cli:** skills binary with init, update and show ([329bf2d](https://github.com/roxblnfk/zed-skills/commit/329bf2d3fba5d06ad72a6265e5879c47fd5b1e1b))
* **core,cli:** directory-level aliases (junction/symlink) with state matrix ([63fb002](https://github.com/roxblnfk/zed-skills/commit/63fb002ddf90a6e5df98de14a0a68c700433bb87))
* **core,providers:** offline cache-only materialize mode ([56e24fb](https://github.com/roxblnfk/zed-skills/commit/56e24fb936f079bd0ad6ca2887edae4067a162dc))
* **core:** audit pipeline config, on-fail aggregation, lockfile verdict cache ([24578ae](https://github.com/roxblnfk/zed-skills/commit/24578ae8be13e027ebdb627259c30e3496a6e03f))
* **core:** domain types, manifest, lockfile, frontmatter reader ([60c8265](https://github.com/roxblnfk/zed-skills/commit/60c8265911855bbec127883cb83dd2c350c355ea))
* **core:** trust model, vendor patterns, discovery gating, partial-run planning ([88cc2fa](https://github.com/roxblnfk/zed-skills/commit/88cc2fa71fe784da31b7e9cd6d2ebcf8f920b584))
* **core:** typed pipeline with transactional non-destructive sync ([794e126](https://github.com/roxblnfk/zed-skills/commit/794e126bc239a102400448d917f6fe02e8180c24))
* **core:** url origin, url provider id, refresh-aware cache plumbing ([aaaa137](https://github.com/roxblnfk/zed-skills/commit/aaaa137f348bcca1aa837d6000b0bf439066b291))
* **lsp,cli:** diagnostics-first LSP server, skills lsp subcommand ([33e54c1](https://github.com/roxblnfk/zed-skills/commit/33e54c110154793b65f1e18f75e157d560335226))
* **providers,audit:** DirProvider, DeclaredLocator, contract testkit, noop audit chain ([3762c0d](https://github.com/roxblnfk/zed-skills/commit/3762c0d09c18afed6f6aec1bcc9c3a0b9ff01648))
* **providers:** ComposerProvider, RecursiveFallbackLocator, treescan ([e44a9d2](https://github.com/roxblnfk/zed-skills/commit/e44a9d2afe5d62d149d33883b7cf455da645f8b1))
* **providers:** remote providers, archive cache, ref resolver, locator chain ([23edacf](https://github.com/roxblnfk/zed-skills/commit/23edacfacf921c647e0eeb76b29f9e94d175e72b))


### Bug Fixes

* **core:** keep malformed-donor warn note ASCII-only ([2edecf9](https://github.com/roxblnfk/zed-skills/commit/2edecf94f99dc71c3ed9e69267c0cfef2dd400ba))
