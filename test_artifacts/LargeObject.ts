// A file with a large object literal to test pair-level splitting

export const LargeConfig = {
  /**
   * First property with a long JSDoc comment.
   * This should be a meaningful split point.
   */
  FIRST_PROPERTY: 'first_value',

  /**
   * Second property with a long JSDoc comment.
   * This should be a meaningful split point.
   */
  SECOND_PROPERTY: 'second_value',

  /**
   * Third property with a long JSDoc comment.
   * This should be a meaningful split point.
   */
  THIRD_PROPERTY: 'third_value',

  /**
   * Fourth property with a long JSDoc comment.
   * This should be a meaningful split point.
   */
  FOURTH_PROPERTY: 'fourth_value',

  /**
   * Fifth property with a long JSDoc comment.
   * This should be a meaningful split point.
   */
  FIFTH_PROPERTY: 'fifth_value',

  /**
   * Sixth property with a long JSDoc comment.
   * This should be a meaningful split point.
   */
  SIXTH_PROPERTY: 'sixth_value',

  /**
   * Seventh property with a long JSDoc comment.
   * This should be a meaningful split point.
   */
  SEVENTH_PROPERTY: 'seventh_value',

  /**
   * Eighth property with a long JSDoc comment.
   * This should be a meaningful split point.
   */
  EIGHTH_PROPERTY: 'eighth_value',

  /**
   * Ninth property with a long JSDoc comment.
   * This should be a meaningful split point.
   */
  NINTH_PROPERTY: 'ninth_value',

  /**
   * Tenth property with a long JSDoc comment.
   * This should be a meaningful split point.
   */
  TENTH_PROPERTY: 'tenth_value',
} as const;
