Fixed
- The memory dashboard's Palaces tree and KG palace picker render again.
  `GET /api/v1/palaces` bridges onto `memory.palaces_list`, which answers
  `{"palaces": [{id, palace, error}]}` where the retired REST route answered a
  bare array — so both views threw `TypeError: S is not iterable` and sat on
  "Loading palaces…". The SPA's api layer now unwraps that wrapper into the
  flat array the views iterate, keeping a palace whose counts could not be read
  visible with its error and unknown ("—") counts
