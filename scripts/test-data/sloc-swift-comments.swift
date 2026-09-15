// A line comment is not code.
/// A doc comment is not code.
import Foundation

/* A block comment
   spanning two lines. */
/* outer /* nested */ still inside the outer comment */
final class Saver { // a trailing comment still counts the code before it
    let count = 1
}
