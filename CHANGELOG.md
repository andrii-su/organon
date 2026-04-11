# 1.0.0 (2026-04-11)


### Features

* add comprehensive tests for embeddings, extractor, indexer, nl_query, and relations modules ([fe517c9](https://github.com/andrii-su/organon/commit/fe517c9d7aea8e0e9ceb83fc9851badc98b27971))
* add explain option to search query and enhance search hit explanations ([9b2e746](https://github.com/andrii-su/organon/commit/9b2e7468dfcf38b1d1f910e479f6f21de5df61d6))
* add extract and store modules for managing file relationships ([edee023](https://github.com/andrii-su/organon/commit/edee023eb02de06577fbd2daa283090cf4a0efe2))
* add functions to retrieve all entries and update paths in the vector store ([2c51482](https://github.com/andrii-su/organon/commit/2c51482f5ca8746b9377470ec382aa3eb8b0ac8f))
* add git integration for file timestamps ([2abf9f5](https://github.com/andrii-su/organon/commit/2abf9f53dccc21d65b89c3dd88d943496a516849))
* add initial implementation of natural-language to SQL query functionality ([4ca59b9](https://github.com/andrii-su/organon/commit/4ca59b971937485f19d060660b9760ee57e9d413))
* add initial implementation of the Organon indexer daemon ([31bc897](https://github.com/andrii-su/organon/commit/31bc89719d989e733e437e7b0b99d2cc8c8d10b6))
* add path reconciliation for lancedb entries after file renames ([ab76dc7](https://github.com/andrii-su/organon/commit/ab76dc7ce81bf7b54d9ff7c1683071e84e869e1a))
* add placeholder for Phase 4 of Rust MCP server ([fd3c617](https://github.com/andrii-su/organon/commit/fd3c617aa6f9dd34e01f50e139d9ccaf163df409))
* add reconcile_lancedb_paths function and related tests for path updates ([956491b](https://github.com/andrii-su/organon/commit/956491bf242a7ce5fe2d652642dfbe0e60d3864b))
* add search explanation structure and integrate into search results ([ab52470](https://github.com/andrii-su/organon/commit/ab5247061455e9d52b8da50ce5b07ceba32c8722))
* add shared utilities and ignored path segments management ([d71bd3c](https://github.com/andrii-su/organon/commit/d71bd3cf67bead3b48a241092f597c1a0446679e))
* add unit tests for rename-detection tracker logic in watcher ([95711b3](https://github.com/andrii-su/organon/commit/95711b3dff79b7e7339b0a48fc138d6f9f13645e))
* **core:** add organon-core crate with PoC file watcher ([e285dea](https://github.com/andrii-su/organon/commit/e285dea6bc9ed324227a71c28711086bb43a51b8))
* enhance entity management with git metadata and lifecycle improvements ([8a89291](https://github.com/andrii-su/organon/commit/8a8929163997b830f1b95451ef5f272ac341b44f))
* enhance indexer with FTS support, summary updates, and improved entity handling ([9108868](https://github.com/andrii-su/organon/commit/9108868b739480750e02d288d26c9fa0a06d3be7))
* enhance MCP server with improved documentation and additional tools for file management ([39759bf](https://github.com/andrii-su/organon/commit/39759bfe4d4417a91861c87f21cd98482dd3d6d7))
* enhance run_once functionality with relation updates, FTS support, and summary storage ([65c341b](https://github.com/andrii-su/organon/commit/65c341bdd5d5296bf71914bd58c1f73ac98cb780))
* enhance tests with improved filtering and ignore set functionality ([ae1de8a](https://github.com/andrii-su/organon/commit/ae1de8adc3000443c4b659d50ba425e3c03fa358))
* enhance text extraction logic with improved error handling and logging ([322c018](https://github.com/andrii-su/organon/commit/322c018c964fa07e8b0edc62961f836bcf5e162d))
* enhance vector store with improved model loading and error handling ([cf0676e](https://github.com/andrii-su/organon/commit/cf0676e4950bb46a9b446c8dae3c17156a7c870c))
* implement delete_relations_from function to remove outgoing edges ([aa294aa](https://github.com/andrii-su/organon/commit/aa294aa877da82f0b19601a29c159739e52f96ef))
* implement format and python modules for timestamp formatting and Python command execution ([f6e51ac](https://github.com/andrii-su/organon/commit/f6e51acb287b046a0fe21bb4e2cd53449241394f))
* improve code formatting and readability in search, graph, watcher, and tests ([e03916c](https://github.com/andrii-su/organon/commit/e03916c7a5793ff137a73c92344ef4ab3fb00488))
* initial project structure ([f970c32](https://github.com/andrii-su/organon/commit/f970c322fa2157926dd1bdeb4c107ba8efb02517))
* refactor embedding model and database path handling for improved configurability ([2f9d93f](https://github.com/andrii-su/organon/commit/2f9d93f75a6b5a06e0de76ddbe302dbb3bbeedb8))
* restructure README for clarity and enhanced documentation ([27137f0](https://github.com/andrii-su/organon/commit/27137f0fb6e88744e9ebf9897190b0b15dfd52eb))
* update .gitignore to include additional directories and file types ([9e88d93](https://github.com/andrii-su/organon/commit/9e88d93329cbd9b93d28cc8db9cdb2b9a7a62805))
* update code structure for improved readability and maintainability ([c8bd5bb](https://github.com/andrii-su/organon/commit/c8bd5bb1ffd688e471b61a471f75e68932c3bf88))
* update project structure with new dependencies and enhanced library functionality ([e41ee00](https://github.com/andrii-su/organon/commit/e41ee0060bc777fec2fa512325abc35ae3a3b848))
* update project structure with new workflows, templates, and configuration files ([1531aef](https://github.com/andrii-su/organon/commit/1531aefad744096c340eb475cfc6c057a638d784))

# Changelog

All notable changes to this project will be documented in this file.

This file is managed by `semantic-release`.
