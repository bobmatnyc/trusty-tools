Fixed

- A finding's reachability now follows the file it sits in, not the words its own text happens to use ([#6082](https://github.com/bobmatnyc/trusty-tools/issues/6082))
  - every finding on a file some finding scopes to localhost inherits that scope
  - a beyond-the-host reach claim about a file the collected data says nothing about is withheld and disclosed, never rewritten
  - a finding is judged by its own component, so an unrelated finding's title words can no longer empty its business impact
- Every Synthesis Status line is now written for the reader ([#6082](https://github.com/bobmatnyc/trusty-tools/issues/6082))
  - the bare `synthesis: available` banner is gone, and the block is omitted when there is nothing to disclose
  - a line about a finding cites the §5.1/§5.2 number the reader can look up instead of naming it by title alone
  - citations for RED findings say section 5.1; they used to say 5.2
