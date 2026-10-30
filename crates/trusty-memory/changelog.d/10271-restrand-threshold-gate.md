Performance
- An upsert into a vector index at or below the exhaustive-scan threshold (24,576 points) no longer re-tests stranded graph points, which cost 3.5-4.5x the insert and is unread at that size. The upsert that takes the index past the threshold rebuilds the stranded set as open does; above it nothing changes ([#10271](https://github.com/bobmatnyc/trusty-tools/issues/10271))
