Fixed
- The pm-guard brace expander now applies its reading cap to comma groups too, so a word of thirty `{a,b}` groups is denied at once instead of expanding 2^30 words and running the hook past its timeout (#8878).
- The secret-read guard now denies a word in an inline program or here-document body that the brace expander cannot resolve (past its cap, or an unbalanced `${`), where it used to allow it (#8878).
