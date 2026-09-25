function applyBulkDiscount(total) {
    if (total > 1000) {
        return total - 50;
    }
    return total;
}

let baseAmount = 1250;
let shippingFee = 250;
let grossTotal = baseAmount + shippingFee;
let netTotal = applyBulkDiscount(grossTotal);

return netTotal;
