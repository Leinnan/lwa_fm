//  Replaces the generated Serde runtime's `BinaryDeserializer` (copied over
//  it by `just apple-typegen`). The generated one makes an array for every
//  number and flag it reads; decoding the view model that way took about a
//  millisecond per refresh for one window. This one reads in place.

import Foundation

public class BinaryDeserializer: Deserializer {
    let input: [UInt8]
    private var location: Int
    private var containerDepthBudget: Int

    init(input: [UInt8], maxContainerDepth: Int) {
        self.input = input
        location = 0
        containerDepthBudget = maxContainerDepth
    }

    /// Moves past `count` bytes; where they start.
    @inline(__always)
    private func take(_ count: Int) throws -> Int {
        let start = location
        guard count >= 0, count <= input.count - start else {
            throw DeserializationError.invalidInput(issue: "Input is too small")
        }
        location = start + count
        return start
    }

    /// A little-endian integer read where it lies.
    @inline(__always)
    private func read<Value: FixedWidthInteger>(_: Value.Type) throws -> Value {
        let start = try take(MemoryLayout<Value>.size)
        return input.withUnsafeBytes { bytes in
            Value(littleEndian: bytes.loadUnaligned(fromByteOffset: start, as: Value.self))
        }
    }

    public func deserialize_len() throws -> Int {
        assertionFailure("Not implemented")
        return 0
    }

    public func deserialize_variant_index() throws -> UInt32 {
        assertionFailure("Not implemented")
        return 0
    }

    public func deserialize_char() throws -> Character {
        throw DeserializationError.invalidInput(issue: "Not implemented: char deserialization")
    }

    public func deserialize_f32() throws -> Float {
        throw DeserializationError.invalidInput(issue: "Not implemented: f32 deserialization")
    }

    public func deserialize_f64() throws -> Double {
        throw DeserializationError.invalidInput(issue: "Not implemented: f64 deserialization")
    }

    public func increase_container_depth() throws {
        if containerDepthBudget == 0 {
            throw DeserializationError.invalidInput(issue: "Exceeded maximum container depth")
        }
        containerDepthBudget -= 1
    }

    public func decrease_container_depth() {
        containerDepthBudget += 1
    }

    public func deserialize_str() throws -> String {
        let count = try deserialize_len()
        let start = try take(count)
        // The core's strings are UTF-8 already; one copy, into the string.
        return String(decoding: input[start..<start + count], as: UTF8.self)
    }

    public func deserialize_bytes() throws -> [UInt8] {
        let count = try deserialize_len()
        let start = try take(count)
        return Array(input[start..<start + count])
    }

    public func deserialize_bool() throws -> Bool {
        switch try deserialize_u8() {
        case 0: return false
        case 1: return true
        case let value:
            throw DeserializationError.invalidInput(issue: "Incorrect value for boolean: \(value)")
        }
    }

    public func deserialize_unit() throws {}

    public func deserialize_u8() throws -> UInt8 {
        input[try take(1)]
    }

    public func deserialize_u16() throws -> UInt16 {
        try read(UInt16.self)
    }

    public func deserialize_u32() throws -> UInt32 {
        try read(UInt32.self)
    }

    public func deserialize_u64() throws -> UInt64 {
        try read(UInt64.self)
    }

    public func deserialize_u128() throws -> UInt128 {
        let low = try deserialize_u64()
        let high = try deserialize_u64()
        return UInt128(high: high, low: low)
    }

    public func deserialize_i8() throws -> Int8 {
        Int8(bitPattern: try deserialize_u8())
    }

    public func deserialize_i16() throws -> Int16 {
        try read(Int16.self)
    }

    public func deserialize_i32() throws -> Int32 {
        try read(Int32.self)
    }

    public func deserialize_i64() throws -> Int64 {
        try read(Int64.self)
    }

    public func deserialize_i128() throws -> Int128 {
        let low = try deserialize_u64()
        let high = try deserialize_i64()
        return Int128(high: high, low: low)
    }

    public func deserialize_option_tag() throws -> Bool {
        switch try deserialize_u8() {
        case 0: return false
        case 1: return true
        case let value:
            throw DeserializationError.invalidInput(issue: "Incorrect value for option tag: \(value)")
        }
    }

    public func get_buffer_offset() -> Int {
        location
    }
}
