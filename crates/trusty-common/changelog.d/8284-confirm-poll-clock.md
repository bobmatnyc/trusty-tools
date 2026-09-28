Fixed
- The search-index registry confirm poll (feature `search-index`) now reads its deadline from an injected clock, so its test drives the poll schedule on a stepped clock instead of the wall clock. The test no longer flakes when one slow registry read under CI load spends the whole deadline (#8284). Production behaviour is unchanged.
