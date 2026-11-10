/**
 * Resolves when the Rust code is ready to call, which it is once the package
 * is imported: Node.js loads the addon while it imports the package, and the
 * import throws when the addon is missing or was built from other sources.
 */
export declare const nativeReady: Promise<void>;
