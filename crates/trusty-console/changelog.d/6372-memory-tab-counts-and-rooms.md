Fixed

- The Memory tab's palace table shows real counts for every palace and a new
  Rooms column. It used to print `—` in every count cell whose row was not
  cache-resident, so on a host with 94 palaces only one showed data. A row now
  renders `—` only when the daemon says the count could not be read, and the
  badge distinguishes "counted on disk" from "unreadable" instead of "not
  loaded" (#6372)
- The headline card reads "Palaces (counted/total)" and a Total Rooms card
  joins the aggregates, matching the totals trusty-memory now sends (#6372)
