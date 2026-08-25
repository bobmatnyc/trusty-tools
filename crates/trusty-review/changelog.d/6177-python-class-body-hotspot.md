Added
A Python class-body hotspot renders as one — "Split oversized class body", remediated by moving cohesive members out — instead of borrowing the function vocabulary and telling a reader to extract the body of a class. This is the Python half of the #6082 whole-impl relabel, and it reads trusty-analyze's new `region_kind` rather than inferring from a missing function name.
